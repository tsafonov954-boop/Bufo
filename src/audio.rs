//! System-audio capture (WASAPI loopback of the default output device).
//!
//! Delivers 48 kHz / 16-bit / stereo PCM chunks over a bounded channel.
//! WASAPI sends nothing while the system is silent, so gaps are padded with
//! zeros against the wall clock to keep the audio timeline continuous.

use std::{
    collections::VecDeque,
    error::Error,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};

use wasapi::{initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

pub const RATE: usize = 48_000;
const BLOCK: usize = 4; // 2 channels * 16-bit

/// Start the capture thread. Returns None (and logs why) if audio is unavailable.
pub fn start() -> Option<Receiver<Vec<u8>>> {
    // ~5 s of buffering at most: if nobody drains (static screen), old audio is dropped.
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(512);
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            if let Err(e) = run(tx, &ready_tx) {
                let _ = ready_tx.send(Err(e.to_string()));
            }
        })
        .ok()?;
    match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Some(rx),
        Ok(Err(e)) => {
            eprintln!("audio disabled: {e}");
            None
        }
        Err(_) => {
            eprintln!("audio disabled: init timed out");
            None
        }
    }
}

fn push(tx: &SyncSender<Vec<u8>>, data: Vec<u8>) -> bool {
    match tx.try_send(data) {
        Ok(()) | Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn run(tx: SyncSender<Vec<u8>>, ready: &mpsc::Sender<Result<(), String>>) -> Result<(), Box<dyn Error>> {
    let _ = initialize_mta();
    let enumerator = DeviceEnumerator::new()?;
    let device = enumerator.get_default_device(Direction::Render)?;
    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(16, 16, SampleType::Int, RATE, 2, None);
    let (_def, min) = client.get_device_period()?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min };
    // Capture direction on a *render* device = loopback.
    client.initialize_client(&format, Direction::Capture, mode, None)?;
    let event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;
    let _ = ready.send(Ok(()));

    let mut queue: VecDeque<u8> = VecDeque::new();
    let started = Instant::now();
    let mut sent_frames: u64 = 0;
    loop {
        let _ = event.wait_for_event(50);
        capture.read_from_device_to_deque(&mut queue)?;
        let got = (queue.len() / BLOCK) as u64;
        if got > 0 {
            let bytes: Vec<u8> = queue.drain(..got as usize * BLOCK).collect();
            sent_frames += got;
            if !push(&tx, bytes) {
                break;
            }
        }
        let expected = started.elapsed().as_micros() as u64 * RATE as u64 / 1_000_000;
        if sent_frames + 960 < expected {
            let pad = (expected - sent_frames).min(RATE as u64);
            sent_frames += pad;
            if !push(&tx, vec![0u8; pad as usize * BLOCK]) {
                break;
            }
        }
    }
    Ok(())
}
