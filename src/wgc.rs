//! Native capture: Windows Graphics Capture -> Media Foundation hardware
//! encoder (HEVC + AAC). Frames never go through ffmpeg. The recording is
//! rotated into short mp4 segments (`seg_XXXXXX.mp4`); a segment is written as
//! `.part` and renamed once finalized, so readers only ever see complete files.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::Receiver,
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

/// Everything needed to create an encoder (cheap to clone into worker threads).
#[derive(Clone)]
pub struct EncCfg {
    pub dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub audio: bool,
}

pub struct WgcFlags {
    pub cfg: EncCfg,
    pub seg_secs: u64,
    pub audio_rx: Option<Receiver<Vec<u8>>>,
}

pub struct Recorder {
    cfg: EncCfg,
    seg_secs: u64,
    audio_rx: Option<Receiver<Vec<u8>>>,
    encoder: Option<VideoEncoder>,
    idx: u64,
    seg_started: Instant,
    /// Next encoder being created on a background thread.
    next: Option<JoinHandle<Result<VideoEncoder, String>>>,
}

fn part_path(dir: &Path, idx: u64) -> PathBuf {
    dir.join(format!("seg_{idx:06}.part"))
}

fn make_encoder(c: &EncCfg, path: &Path) -> Result<VideoEncoder, String> {
    VideoEncoder::new(
        VideoSettingsBuilder::new(c.width, c.height).bitrate(c.bitrate).frame_rate(c.fps),
        AudioSettingsBuilder::default().disabled(!c.audio),
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

impl Recorder {
    /// A recorder fed by something other than WGC (see dda.rs).
    pub fn standalone(cfg: EncCfg, seg_secs: u64, audio_rx: Option<Receiver<Vec<u8>>>) -> Result<Self, String> {
        let encoder = make_encoder(&cfg, &part_path(&cfg.dir, 0))?;
        Ok(Self { cfg, seg_secs, audio_rx, encoder: Some(encoder), idx: 0, seg_started: Instant::now(), next: None })
    }

    /// Start the next segment's encoder ahead of time and switch when the
    /// current segment is full.
    pub fn rotate(&mut self) {
        let seg = Duration::from_secs(self.seg_secs);
        let el = self.seg_started.elapsed();

        if self.next.is_none() && el >= seg.mul_f32(0.7) {
            let cfg = self.cfg.clone();
            let path = part_path(&cfg.dir, self.idx + 1);
            self.next = Some(thread::spawn(move || make_encoder(&cfg, &path)));
        }

        if el >= seg {
            if let Some(handle) = self.next.take() {
                match handle.join() {
                    Ok(Ok(new_enc)) => {
                        if let Some(old) = self.encoder.replace(new_enc) {
                            finish_async(old, part_path(&self.cfg.dir, self.idx));
                        }
                        self.idx += 1;
                        self.seg_started = Instant::now();
                    }
                    Ok(Err(e)) => eprintln!("next encoder failed: {e}"),
                    Err(_) => eprintln!("encoder thread panicked"),
                }
            }
        }
    }

    fn drain_audio(enc: &mut VideoEncoder, rx: &Option<Receiver<Vec<u8>>>) {
        if let Some(rx) = rx {
            while let Ok(chunk) = rx.try_recv() {
                if let Err(e) = enc.send_audio_buffer(&chunk, 0) {
                    crate::settings::log(&format!("audio send failed: {e}"));
                    break;
                }
            }
        }
    }

    /// Send a GPU texture without a CPU copy (`ts` in 100 ns units).
    pub fn push_texture(&mut self, tex: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, ts: i64) {
        self.rotate();
        if let Some(enc) = self.encoder.as_mut() {
            Self::drain_audio(enc, &self.audio_rx);
            if let Err(e) = enc.send_texture(tex, ts) {
                crate::settings::log(&format!("frame send failed: {e}"));
            }
        }
    }

    pub fn close(&mut self) {
        if let Some(enc) = self.encoder.take() {
            finish_async(enc, part_path(&self.cfg.dir, self.idx));
        }
    }
}

impl GraphicsCaptureApiHandler for Recorder {
    type Flags = WgcFlags;
    type Error = Err;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let WgcFlags { cfg, seg_secs, audio_rx } = ctx.flags;
        let encoder = make_encoder(&cfg, &part_path(&cfg.dir, 0))?;
        Ok(Self {
            cfg,
            seg_secs,
            audio_rx,
            encoder: Some(encoder),
            idx: 0,
            seg_started: Instant::now(),
            next: None,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        self.rotate();

        if let Some(enc) = self.encoder.as_mut() {
            Self::drain_audio(enc, &self.audio_rx);
            let t0 = Instant::now();
            if let Err(e) = enc.send_frame(frame) {
                crate::settings::log(&format!("frame send failed: {e}"));
            }
            if std::env::var_os("BUFO_DEBUG").is_some() {
                crate::settings::log(&format!("frame idx {} took {} ms", self.idx, t0.elapsed().as_millis()));
            }
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        if let Some(enc) = self.encoder.take() {
            finish_async(enc, part_path(&self.cfg.dir, self.idx));
        }
        Ok(())
    }
}

pub type Control = CaptureControl<Recorder, Err>;

/// Start capturing the primary monitor on a background thread.
pub fn start(
    dir: PathBuf,
    fps: u32,
    bitrate: u32,
    seg_secs: u64,
    audio_rx: Option<Receiver<Vec<u8>>>,
) -> Result<Control, Err> {
    // Experiment: BUFO_WINDOW=<title part> captures that window instead of the monitor.
    if let Ok(title) = std::env::var("BUFO_WINDOW") {
        let win = windows_capture::window::Window::from_contains_name(&title)?;
        let (width, height) = (win.width()? as u32 & !1, win.height()? as u32 & !1);
        let cfg = EncCfg { dir, width, height, fps, bitrate, audio: audio_rx.is_some() };
        let flags = WgcFlags { cfg, seg_secs, audio_rx };
        let settings = Settings::new(
            win,
            CursorCaptureSettings::WithCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        );
        return Ok(Recorder::start_free_threaded(settings)?);
    }
    let monitor = Monitor::primary()?;
    let (width, height) = (monitor.width()?, monitor.height()?);
    let cfg = EncCfg { dir, width, height, fps, bitrate, audio: audio_rx.is_some() };
    let flags = WgcFlags { cfg, seg_secs, audio_rx };
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
