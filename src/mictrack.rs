//! Separate microphone track. The audio thread writes the mic as raw 48 kHz /
//! 16-bit / stereo PCM files `seg_mic_<unix ms>.pcm` (the name is the time of
//! the first frame). When a clip is saved, the slice of that PCM matching the
//! clip's time window is cut out and muxed as a second audio track, so the
//! voice can be raised, lowered or removed when editing.

use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{SystemTime, UNIX_EPOCH},
};

pub const RATE: u64 = 48_000;
const FRAME: u64 = 4; // bytes per stereo 16-bit frame

pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Appends PCM to rotating files named after their start time.
pub struct Writer {
    dir: PathBuf,
    file: Option<File>,
    frames_in_file: u64,
    chunk_frames: u64,
}

impl Writer {
    pub fn new(dir: PathBuf, chunk_secs: u64) -> Self {
        Self { dir, file: None, frames_in_file: 0, chunk_frames: chunk_secs.max(1) * RATE }
    }

    /// `start_ms` is the wall-clock time of the first frame in `pcm`.
    pub fn write(&mut self, start_ms: u64, pcm: &[u8]) {
        if self.file.is_none() || self.frames_in_file >= self.chunk_frames {
            self.file = File::create(self.dir.join(format!("seg_mic_{start_ms}.pcm"))).ok();
            self.frames_in_file = 0;
        }
        if let Some(f) = &mut self.file {
            let _ = f.write_all(pcm);
            self.frames_in_file += pcm.len() as u64 / FRAME;
        }
    }
}

fn start_of(p: &Path) -> Option<u64> {
    p.file_name()?.to_str()?.strip_prefix("seg_mic_")?.strip_suffix(".pcm")?.parse().ok()
}

fn files(dir: &Path) -> Vec<(u64, PathBuf)> {
    let mut v: Vec<(u64, PathBuf)> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| Some((start_of(&e.path())?, e.path()))).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Delete mic files that ended before `cutoff_ms`.
pub fn prune(dir: &Path, cutoff_ms: u64) {
    for (start, p) in files(dir) {
        if start < cutoff_ms {
            let _ = fs::remove_file(p);
        }
    }
}

fn zeros(out: &mut impl Write, mut frames: u64) -> std::io::Result<()> {
    let buf = vec![0u8; 64 * 1024];
    while frames > 0 {
        let n = (frames * FRAME).min(buf.len() as u64);
        out.write_all(&buf[..n as usize])?;
        frames -= n / FRAME;
    }
    Ok(())
}

/// Write a WAV with the mic audio for `[t0_ms, t1_ms)`; gaps become silence.
/// Returns false when there is no mic audio in that window at all.
pub fn make_wav(dir: &Path, t0_ms: u64, t1_ms: u64, wav: &Path) -> bool {
    if t1_ms <= t0_ms {
        return false;
    }
    let per_ms = RATE / 1000;
    let (begin, end) = (t0_ms * per_ms, t1_ms * per_ms);
    let list = files(dir);
    let mut any = false;
    let Ok(f) = File::create(wav) else { return false };
    let mut out = std::io::BufWriter::new(f);
    let data = (end - begin) * FRAME;
    let mut hdr = Vec::with_capacity(44);
    hdr.extend(b"RIFF");
    hdr.extend(((36 + data) as u32).to_le_bytes());
    hdr.extend(b"WAVEfmt ");
    hdr.extend(16u32.to_le_bytes());
    hdr.extend(1u16.to_le_bytes()); // PCM
    hdr.extend(2u16.to_le_bytes());
    hdr.extend((RATE as u32).to_le_bytes());
    hdr.extend((RATE as u32 * FRAME as u32).to_le_bytes());
    hdr.extend((FRAME as u16).to_le_bytes());
    hdr.extend(16u16.to_le_bytes());
    hdr.extend(b"data");
    hdr.extend((data as u32).to_le_bytes());
    if out.write_all(&hdr).is_err() {
        return false;
    }
    let mut cur = begin;
    for (start_ms, p) in list {
        let s = start_ms * per_ms;
        let Ok(mut f) = File::open(&p) else { continue };
        let len = f.metadata().map(|m| m.len() / FRAME).unwrap_or(0);
        if s >= end {
            break;
        }
        if s + len <= cur {
            continue;
        }
        if s > cur && zeros(&mut out, s - cur).is_err() {
            return false;
        }
        cur = cur.max(s);
        let take = end.min(s + len) - cur;
        if f.seek(SeekFrom::Start((cur - s) * FRAME)).is_err() {
            continue;
        }
        let mut chunk = vec![0u8; (take * FRAME) as usize];
        if f.read_exact(&mut chunk).is_err() || out.write_all(&chunk).is_err() {
            return false;
        }
        any = true;
        cur += take;
    }
    if zeros(&mut out, end - cur).is_err() || out.flush().is_err() {
        return false;
    }
    any
}

/// Add the mic as a second audio track to `clip`, aligned so the clip ends at
/// `end_ms`. `clip_secs` is the clip's duration. Leaves the clip untouched on failure.
pub fn add_track(ffmpeg: &str, buffer_dir: &Path, clip: &Path, end_ms: u64, clip_secs: f64) -> bool {
    let t0 = end_ms.saturating_sub((clip_secs * 1000.0) as u64);
    let wav = clip.with_extension("mic.wav");
    let tmp = clip.with_extension("mux.mp4");
    let mut ok = make_wav(buffer_dir, t0, end_ms, &wav);
    if ok {
        let mut cmd = crate::platform::command(ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(clip).arg("-i").arg(&wav);
        cmd.args(["-map", "0:v", "-map", "0:a?", "-map", "1:a", "-c", "copy", "-c:a:1", "aac", "-b:a:1", "160k"]);
        cmd.args(["-metadata:s:a:0", "title=Game", "-metadata:s:a:1", "title=Microphone", "-movflags", "+faststart"]);
        cmd.arg(&tmp).stdin(Stdio::null());
        crate::platform::no_window(&mut cmd);
        ok = cmd.status().is_ok_and(|s| s.success()) && fs::rename(&tmp, clip).is_ok();
    }
    let _ = fs::remove_file(&wav);
    let _ = fs::remove_file(&tmp);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_has_silence_for_gaps_and_slices_files() {
        let dir = std::env::temp_dir().join(format!("bufo_mic_test_{}", unix_ms()));
        fs::create_dir_all(&dir).unwrap();
        // 100 ms of 0x01 samples starting at t=1000 ms
        fs::write(dir.join("seg_mic_1000.pcm"), vec![1u8; 4800 * 4]).unwrap();
        let wav = dir.join("o.wav");
        assert!(make_wav(&dir, 950, 1200, &wav)); // 50 ms gap, 100 ms audio, 100 ms gap
        let b = fs::read(&wav).unwrap();
        assert_eq!(b.len(), 44 + 250 * 48 * 4);
        let pcm = &b[44..];
        assert_eq!(pcm[0], 0);
        assert_eq!(pcm[2400 * 4], 1); // first audible frame after the 50 ms gap
        assert_eq!(pcm[(2400 + 4800) * 4], 0);
        assert!(!make_wav(&dir, 5000, 6000, &dir.join("e.wav")));
        let _ = fs::remove_dir_all(&dir);
    }
}
