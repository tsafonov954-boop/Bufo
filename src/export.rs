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

/// Length of a video in seconds, read from `ffmpeg -i` output.
pub fn duration(input: &Path) -> Option<f64> {
    let out = Command::new(ffmpeg_bin()).args(["-hide_banner", "-i"]).arg(input).stdin(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let rest = text.split("Duration:").nth(1)?.trim_start();
    let t = rest.split(',').next()?.trim();
    let mut parts = t.split(':');
    let (h, m, s): (f64, f64, f64) =
        (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Write `n` evenly spaced small JPEG frames of a clip to `dir/<prefix>_<k>.jpg`
/// (for the trim editor's film strip). Existing frames are kept.
pub fn filmstrip(input: &Path, dur: f64, n: usize, dir: &Path, prefix: &str) {
    let _ = fs::create_dir_all(dir);
    for k in 0..n {
        let out = dir.join(format!("{prefix}_{k}.jpg"));
        if out.exists() {
            continue;
        }
        // Stay a little inside the clip: the last video frame is often earlier than the container length.
        let t = (dur * (k as f64 + 0.5) / n as f64).min((dur - 0.5).max(0.0));
        let mut cmd = Command::new(ffmpeg_bin());
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{t:.2}"), "-i"]).arg(input);
        cmd.args(["-frames:v", "1", "-vf", "scale=200:-2,format=yuvj420p", "-q:v", "6"]).arg(&out);
        cmd.stdin(Stdio::null());
        crate::platform::no_window(&mut cmd);
        let _ = cmd.status();
    }
}

/// Run ffmpeg with the video encoder `enc` (GPU first, software as fallback).
fn run_with_fallback(build: impl Fn(&[&str]) -> Command) -> bool {
    let attempts: [&[&str]; 2] = [
        &["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "21", "-b:v", "0"],
        &["-c:v", "libx264", "-preset", "veryfast", "-crf", "21"],
    ];
    attempts.iter().any(|enc| build(enc).stdin(Stdio::null()).status().is_ok_and(|s| s.success()))
}

/// Cut `[from, to]` seconds out of a clip, frame-accurately (re-encodes video, keeps audio).
/// Writes `<name>_trim.mp4` next to the input and returns its path.
pub fn trim(input: &Path, from: f64, to: f64) -> Option<PathBuf> {
    if to <= from {
        return None;
    }
    let stem = input.file_stem()?.to_string_lossy().to_string();
    let out = input.with_file_name(format!("{stem}_trim.mp4"));
    let ok = run_with_fallback(|enc| {
        let mut cmd = Command::new(ffmpeg_bin());
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(input);
        cmd.args(["-ss", &format!("{from:.3}"), "-to", &format!("{to:.3}")]);
        cmd.args(enc).args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"]);
        cmd.arg(&out);
        cmd
    });
    ok.then_some(out)
}

/// Animated GIF (max 15 s, 480 px wide, 15 fps) from `[from, to]` seconds.
pub fn gif(input: &Path, from: f64, to: f64) -> Option<PathBuf> {
    let to = to.min(from + 15.0);
    if to <= from {
        return None;
    }
    let stem = input.file_stem()?.to_string_lossy().to_string();
    let out = input.with_file_name(format!("{stem}.gif"));
    let ok = Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-ss", &format!("{from:.3}"), "-to", &format!("{to:.3}"), "-i"])
        .arg(input)
        .args([
            "-vf",
            "fps=15,scale=480:-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
            "-an",
        ])
        .arg(&out)
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(out)
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
