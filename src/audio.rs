//! Audio capture: system audio (WASAPI loopback of the default output device)
//! and, optionally, the default microphone, mixed into one 48 kHz / 16-bit /
//! stereo PCM stream delivered over a bounded channel.
//!
//! Both sources are paced against the wall clock: WASAPI sends nothing while a
//! source is silent, so missing audio is filled with zeros to keep the
//! timeline continuous. Output lags the clock by a small fixed delay so the
//! slower source has time to deliver.

use std::{
    collections::VecDeque,
    error::Error,
    path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};

use wasapi::{
    initialize_mta, AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, SampleType,
    StreamMode, WaveFormat,
};

pub const RATE: usize = 48_000;
const BLOCK: usize = 4; // 2 channels * 16-bit
const DELAY_MS: u128 = 60;
/// Max backlog kept per source (~500 ms); older audio is dropped to stay in sync.
const MAX_BACKLOG: usize = RATE / 2 * BLOCK;

struct Source {
    _client: AudioClient,
    capture: AudioCaptureClient,
    queue: VecDeque<u8>,
}

fn open_source(
    enumerator: &DeviceEnumerator,
    device_dir: Direction,
) -> Result<Source, Box<dyn Error>> {
    let device = enumerator.get_default_device(device_dir)?;
    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(16, 16, SampleType::Int, RATE, 2, None);
    let (_def, min) = client.get_device_period()?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min };
    // Capture stream; on a *render* device this is loopback.
    client.initialize_client(&format, Direction::Capture, mode, None)?;
    let _event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;
    Ok(Source { _client: client, capture, queue: VecDeque::new() })
}

/// Start the capture thread. Returns None (and logs why) if audio is unavailable.
/// With `mic_dir` set, the microphone is not mixed in but written there as a
/// separate track (see `mictrack`).
pub fn start(mic: bool, mic_dir: Option<PathBuf>) -> Option<Receiver<Vec<u8>>> {
    // ~5 s of buffering at most: if nobody drains (static screen), old audio is dropped.
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(512);
    let (ready_tx, ready_rx) = mpsc::channel::<Result<String, String>>();
    thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            if let Err(e) = run(tx, &ready_tx, mic, mic_dir) {
                let _ = ready_tx.send(Err(e.to_string()));
            }
        })
        .ok()?;
    match ready_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(desc)) => {
            println!("audio: {desc}");
            Some(rx)
        }
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

fn run(
    tx: SyncSender<Vec<u8>>,
    ready: &mpsc::Sender<Result<String, String>>,
    mic: bool,
    mic_dir: Option<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let _ = initialize_mta();
    let enumerator = DeviceEnumerator::new()?;
    let mut sources = vec![open_source(&enumerator, Direction::Render)?];
    let mut desc = String::from("system loopback");
    let mut mic_src: Option<Source> = None;
    if mic {
        match open_source(&enumerator, Direction::Capture) {
            Ok(s) => {
                if mic_dir.is_some() {
                    mic_src = Some(s);
                    desc.push_str(" + microphone (separate track)");
                } else {
                    sources.push(s);
                    desc.push_str(" + microphone");
                }
            }
            Err(e) => eprintln!("microphone unavailable ({e}), system audio only"),
        }
    }
    let _ = ready.send(Ok(desc));

    let started = Instant::now();
    let started_ms = crate::mictrack::unix_ms();
    let mut writer = mic_dir.map(|d| crate::mictrack::Writer::new(d, crate::seg_secs()));
    let mut sent_frames: u64 = 0;
    loop {
        thread::sleep(Duration::from_millis(10));
        for s in sources.iter_mut().chain(mic_src.iter_mut()) {
            s.capture.read_from_device_to_deque(&mut s.queue)?;
            if s.queue.len() > MAX_BACKLOG {
                let drop = (s.queue.len() - MAX_BACKLOG) / BLOCK * BLOCK;
                s.queue.drain(..drop);
            }
        }
        let target = (started.elapsed().as_millis().saturating_sub(DELAY_MS) as u64) * RATE as u64 / 1000;
        if target < sent_frames + 480 {
            continue; // emit in chunks of at least 10 ms
        }
        let n = (target - sent_frames) as usize;
        let mut mix = vec![0i16; n * 2];
        for s in sources.iter_mut() {
            let take = n.min(s.queue.len() / BLOCK) * 2;
            for sample in mix.iter_mut().take(take) {
                let lo = s.queue.pop_front().unwrap_or(0);
                let hi = s.queue.pop_front().unwrap_or(0);
                *sample = sample.saturating_add(i16::from_le_bytes([lo, hi]));
            }
        }
        if let (Some(m), Some(w)) = (mic_src.as_mut(), writer.as_mut()) {
            let mut pcm = vec![0u8; n * BLOCK];
            let take = n.min(m.queue.len() / BLOCK) * BLOCK;
            for b in pcm.iter_mut().take(take) {
                *b = m.queue.pop_front().unwrap_or(0);
            }
            w.write(started_ms + sent_frames * 1000 / RATE as u64, &pcm);
        }
        let bytes: Vec<u8> = mix.iter().flat_map(|v| v.to_le_bytes()).collect();
        sent_frames += n as u64;
        if !push(&tx, bytes) {
            break;
        }
    }
    Ok(())
}
