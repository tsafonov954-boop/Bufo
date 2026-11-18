//! Native capture: Windows Graphics Capture -> Media Foundation hardware
//! encoder (HEVC). Frames never go through ffmpeg. The recording is rotated
//! into short mp4 segments (`seg_XXXXXX.mp4`); a segment is written as
//! `.part` and renamed once finalized, so readers only ever see complete files.

use std::{
    fs,
    path::{Path, PathBuf},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    encoder::{
        AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    },
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

pub type Err = Box<dyn std::error::Error + Send + Sync>;

pub struct WgcFlags {
    pub dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub seg_secs: u64,
}

pub struct Recorder {
    flags: WgcFlags,
    encoder: Option<VideoEncoder>,
    idx: u64,
    seg_started: Instant,
    /// Next encoder being created on a background thread.
    next: Option<JoinHandle<Result<VideoEncoder, String>>>,
}

fn part_path(dir: &Path, idx: u64) -> PathBuf {
    dir.join(format!("seg_{idx:06}.part"))
}

fn make_encoder(f: &WgcFlags, path: &Path) -> Result<VideoEncoder, String> {
    VideoEncoder::new(
        VideoSettingsBuilder::new(f.width, f.height).bitrate(f.bitrate).frame_rate(f.fps),
        AudioSettingsBuilder::default().disabled(true),
        ContainerSettingsBuilder::default(),
        path,
    )
    .map_err(|e| e.to_string())
}

/// Finalize an encoder off the capture thread, then publish the file.
fn finish_async(enc: VideoEncoder, part: PathBuf) {
    thread::spawn(move || {
        if enc.finish().is_ok() {
            let _ = fs::rename(&part, part.with_extension("mp4"));
        }
    });
}

impl GraphicsCaptureApiHandler for Recorder {
    type Flags = WgcFlags;
    type Error = Err;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let f = ctx.flags;
        let encoder = make_encoder(&f, &part_path(&f.dir, 0))?;
        Ok(Self { flags: f, encoder: Some(encoder), idx: 0, seg_started: Instant::now(), next: None })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let seg = Duration::from_secs(self.flags.seg_secs);
        let el = self.seg_started.elapsed();
        if std::env::var_os("CR_DEBUG").is_some() {
            eprintln!("frame idx={} el={:?}", self.idx, el);
        }

        // Prepare the next segment's encoder ahead of time.
        if self.next.is_none() && el >= seg.mul_f32(0.7) {
            let (dir, idx) = (self.flags.dir.clone(), self.idx + 1);
            let f = WgcFlags { dir: dir.clone(), ..self.flags_copy() };
            self.next = Some(thread::spawn(move || make_encoder(&f, &part_path(&dir, idx))));
        }

        if el >= seg {
            if let Some(handle) = self.next.take() {
                match handle.join() {
                    Ok(Ok(new_enc)) => {
                        if let Some(old) = self.encoder.replace(new_enc) {
                            finish_async(old, part_path(&self.flags.dir, self.idx));
                        }
                        self.idx += 1;
                        self.seg_started = Instant::now();
                    }
                    Ok(Err(e)) => eprintln!("next encoder failed: {e}"),
                    Err(_) => eprintln!("encoder thread panicked"),
                }
            }
        }

        if let Some(enc) = self.encoder.as_mut() {
            enc.send_frame(frame)?;
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        if let Some(enc) = self.encoder.take() {
            finish_async(enc, part_path(&self.flags.dir, self.idx));
        }
        Ok(())
    }
}

impl Recorder {
    fn flags_copy(&self) -> WgcFlags {
        WgcFlags {
            dir: self.flags.dir.clone(),
            width: self.flags.width,
            height: self.flags.height,
            fps: self.flags.fps,
            bitrate: self.flags.bitrate,
            seg_secs: self.flags.seg_secs,
        }
    }
}

pub type Control = CaptureControl<Recorder, Err>;

/// Start capturing the primary monitor on a background thread.
pub fn start(dir: PathBuf, fps: u32, bitrate: u32, seg_secs: u64) -> Result<Control, Err> {
    let monitor = Monitor::primary()?;
    let (width, height) = (monitor.width()?, monitor.height()?);
    let flags = WgcFlags { dir, width, height, fps, bitrate, seg_secs };
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    Ok(Recorder::start_free_threaded(settings)?)
}
