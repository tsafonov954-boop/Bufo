//! Export a clip as a vertical (9:16, 1080x1920) video for TikTok / Shorts /
//! Reels, optionally trimmed. Runs ffmpeg with the GPU H.264 encoder.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{ffmpeg_bin, Config};

#[derive(Clone, Copy, PartialEq)]
pub enum Style {
    /// Whole frame in the middle over a blurred, enlarged copy of itself.
    Blur,
    /// Centre crop to 9:16 (loses the sides).
    Crop,
}

impl Style {
    pub fn parse(s: &str) -> Self {
        if s.eq_ignore_ascii_case("crop") {
            Style::Crop
        } else {
            Style::Blur
        }
    }

    fn filter(self) -> &'static str {
        match self {
            Style::Blur => {
                "[0:v]split[a][b];\
                 [a]scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920,boxblur=30:5[bg];\
                 [b]scale=1080:-2[fg];\
                 [bg][fg]overlay=(W-w)/2:(H-h)/2,format=yuv420p[v]"
            }
            Style::Crop => "[0:v]crop=ih*9/16:ih,scale=1080:1920,format=yuv420p[v]",
        }
    }
}

/// Newest `clip_*.mp4` in the clips folder (what "export last clip" means).
pub fn newest_clip(cfg: &Config) -> Option<PathBuf> {
    fs::read_dir(&cfg.clips_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("clip_") && n.ends_with(".mp4")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// Re-encode `input` to 1080x1920. `from`/`to` are seconds (None = whole clip).
/// Returns the output path (`export_<name>_vertical.mp4`, next to the input).
pub fn vertical(input: &Path, from: Option<f64>, to: Option<f64>, style: Style) -> Option<PathBuf> {
    let stem = input.file_stem()?.to_string_lossy().to_string();
    let out = input.with_file_name(format!("export_{stem}_vertical.mp4"));
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if let Some(s) = from {
        cmd.args(["-ss", &s.to_string()]);
    }
    if let Some(t) = to {
        cmd.args(["-to", &t.to_string()]);
    }
    cmd.arg("-i").arg(input);
    cmd.args(["-filter_complex", style.filter(), "-map", "[v]", "-map", "0:a?"]);
    cmd.args(["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "23", "-b:v", "0"]);
    cmd.args(["-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"]);
    cmd.arg(&out).stdin(Stdio::null());
    let ok = cmd.status().ok()?.success();
    if ok {
        return Some(out);
    }
    // No NVENC (e.g. a non-NVIDIA machine): fall back to software x264.
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if let Some(s) = from {
        cmd.args(["-ss", &s.to_string()]);
    }
    if let Some(t) = to {
        cmd.args(["-to", &t.to_string()]);
    }
    cmd.arg("-i").arg(input);
    cmd.args(["-filter_complex", style.filter(), "-map", "[v]", "-map", "0:a?"]);
    cmd.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"]);
    cmd.args(["-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"]);
    cmd.arg(&out).stdin(Stdio::null());
    cmd.status().ok()?.success().then_some(out)
}
