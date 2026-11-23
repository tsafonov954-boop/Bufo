//! Bufo.
//!
//! Windows Graphics Capture feeds a hardware HEVC encoder that writes short
//! segments to a ring folder on disk, so RAM stays small. A hotkey (or the
//! tray menu) concatenates the newest segments into an mp4 with `-c copy`
//! (no re-encode, instant).
//!
//! Hotkeys: Alt+F10 = save clip, Alt+F11 = quit. Settings: see `settings.rs`.

// Release builds run without a console window; the tray icon is the UI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[cfg(windows)]
use std::sync::mpsc::Receiver;
#[cfg(windows)]
use std::time::Instant;

#[cfg(windows)]
mod audio;
mod mictrack;
#[cfg(windows)]
mod dda;
mod export;
mod gallery;
mod platform;
mod settings;
mod ui;
#[cfg(windows)]
mod wgc;
#[cfg(windows)]
mod winfo;
#[cfg(not(windows))]
mod winfo {
    //! Foreground-app detection is not implemented on this platform yet.
    #[allow(dead_code)]
    pub fn start_tracker() {}
    pub fn last_app() -> String {
        String::new()
    }
}

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::GetLastError,
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT},
        WindowsAndMessaging::{DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_HOTKEY, WM_QUIT},
    },
};
#[cfg(windows)]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIconBuilder,
};

/// Segment length in seconds (default 8, `--seg N` overrides). Longer segments
/// mean fewer encoder restarts, which is what leaks a few handles each time.
static SEG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(8);

fn seg_secs() -> u64 {
    SEG.load(std::sync::atomic::Ordering::Relaxed).max(1)
}

#[derive(Clone)]
struct Config {
    buffer_dir: PathBuf,
    clips_dir: PathBuf,
    buffer_secs: u64,
    clip_secs: u64,
    fps: u32,
    cq: u32,
    bitrate: u32,
    audio: bool,
    /// Mix the default microphone into the audio track (opt-in, `--mic`).
    mic: bool,
    /// Write the microphone as its own audio track instead of mixing it in.
    mic_track: bool,
    /// Put the saved clip file on the clipboard (paste into Discord, Telegram...).
    clipboard: bool,
    /// Clip folder size limit in GB, 0 = unlimited.
    max_clips_gb: f64,
    /// Delete clips older than this many days, 0 = keep forever.
    keep_days: u64,
    hotkey_save: String,
    hotkey_quit: String,
    hotkey_shot: String,
    hotkey_long: String,
    clip_long_secs: u64,
    /// Test/maintenance mode: only run the clip cleanup, then exit.
    cleanup_only: bool,
    /// Build the gallery page and exit (`--gallery` also opens it).
    gallery: bool,
    gallery_open: bool,
    /// `--ui`: open the application window instead of recording.
    ui: bool,
    /// `--export FILE` (or `--export last`): make a 9:16 version and exit.
    export: Option<String>,
    export_style: String,
    /// `--trim FILE|last --from S --to S` or `--gif FILE|last`: edit a clip and exit.
    edit: Option<(&'static str, String)>,
    /// `--screenshot`: take one screenshot and exit.
    shot_only: bool,
    export_from: Option<f64>,
    export_to: Option<f64>,
    /// "auto" (default: ffmpeg ddagrab + NVENC when available, lowest CPU), "dda" (built-in
    /// Media Foundation encoder), "wgc" or "ffmpeg".
    capture: String,
    /// Test mode: save one clip after N seconds, then exit.
    save_after: Option<u64>,
}

fn parse_args() -> Config {
    let base = env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("Videos");
    // Keep using the folder from the pre-rename "ClipRecorder" builds if it exists.
    let base = if base.join("Bufo").exists() || !base.join("ClipRecorder").exists() {
        base.join("Bufo")
    } else {
        base.join("ClipRecorder")
    };
    let mut c = Config {
        buffer_dir: env::temp_dir().join("bufo-buffer"),
        clips_dir: base,
        buffer_secs: 900,
        clip_secs: 60,
        fps: 60,
        cq: 27,
        bitrate: 6_000_000,
        audio: true,
        mic: false,
        mic_track: true,
        clipboard: true,
        max_clips_gb: 0.0,
        keep_days: 0,
        hotkey_save: platform::DEFAULT_HOTKEY_SAVE.to_string(),
        hotkey_quit: platform::DEFAULT_HOTKEY_QUIT.to_string(),
        hotkey_shot: platform::DEFAULT_HOTKEY_SHOT.to_string(),
        hotkey_long: platform::DEFAULT_HOTKEY_LONG.to_string(),
        clip_long_secs: 300,
        cleanup_only: false,
        gallery: false,
        gallery_open: false,
        ui: false,
        export: None,
        export_style: "blur".to_string(),
        edit: None,
        shot_only: false,
        export_from: None,
        export_to: None,
        capture: "auto".to_string(),
        save_after: None,
    };
    // Settings file overrides the defaults; command-line flags override both.
    let f = settings::load();
    if let Some(v) = f.clips_dir {
        c.clips_dir = PathBuf::from(v);
    }
    c.buffer_secs = f.buffer_secs.unwrap_or(c.buffer_secs);
    c.clip_secs = f.clip_secs.unwrap_or(c.clip_secs);
    c.fps = f.fps.unwrap_or(c.fps);
    c.bitrate = f.bitrate.unwrap_or(c.bitrate);
    c.audio = f.audio.unwrap_or(c.audio);
    c.mic = f.mic.unwrap_or(c.mic);
    c.mic_track = f.mic_track.unwrap_or(c.mic_track);
    c.clipboard = f.clipboard.unwrap_or(c.clipboard);
    c.max_clips_gb = f.max_clips_gb.unwrap_or(c.max_clips_gb);
    c.keep_days = f.keep_days.unwrap_or(c.keep_days);
    c.hotkey_save = f.hotkey_save.unwrap_or(c.hotkey_save);
    c.hotkey_quit = f.hotkey_quit.unwrap_or(c.hotkey_quit);
    c.hotkey_shot = f.hotkey_shot.unwrap_or(c.hotkey_shot);
    c.hotkey_long = f.hotkey_long.unwrap_or(c.hotkey_long);
    c.clip_long_secs = f.clip_long_secs.unwrap_or(c.clip_long_secs);
    if let Some(s) = f.segment_secs {
        SEG.store(s, std::sync::atomic::Ordering::Relaxed);
    }
    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let val = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--buffer" => c.buffer_secs = val.parse().unwrap_or(c.buffer_secs),
            "--clip" => c.clip_secs = val.parse().unwrap_or(c.clip_secs),
            "--fps" => c.fps = val.parse().unwrap_or(c.fps),
            "--cq" => c.cq = val.parse().unwrap_or(c.cq),
            "--bitrate" => c.bitrate = val.parse().unwrap_or(c.bitrate),
            "--capture" => c.capture = val,
            "--seg" => SEG.store(val.parse().unwrap_or(8), std::sync::atomic::Ordering::Relaxed),
            "--no-audio" => {
                c.audio = false;
                i += 1;
                continue;
            }
            "--mic" => {
                c.mic = true;
                i += 1;
                continue;
            }
            "--max-gb" => c.max_clips_gb = val.parse().unwrap_or(c.max_clips_gb),
            "--keep-days" => c.keep_days = val.parse().unwrap_or(c.keep_days),
            "--gallery" | "--gallery-build" => {
                c.gallery = true;
                c.gallery_open = args[i] == "--gallery";
                i += 1;
                continue;
            }
            "--ui" => {
                c.ui = true;
                i += 1;
                continue;
            }
            "--export" => c.export = Some(val),
            "--style" => c.export_style = val,
            "--trim" => c.edit = Some(("trim", val)),
            "--gif" => c.edit = Some(("gif", val)),
            "--from" => c.export_from = val.parse().ok(),
            "--to" => c.export_to = val.parse().ok(),
            "--cleanup" => {
                c.cleanup_only = true;
                i += 1;
                continue;
            }
            "--screenshot" => {
                c.shot_only = true;
                i += 1;
                continue;
            }
            "--restart" => {
                // Restart the background recorder (applies changed settings).
                println!("restart: {}", if platform::restart_recorder() { "ok" } else { "failed" });
                std::process::exit(0);
            }
            "--autostart" => {
                // --autostart on | off | status
                let ok = match val.as_str() {
                    "on" => platform::set_autostart(true),
                    "off" => platform::set_autostart(false),
                    _ => true,
                };
                println!("autostart: {} ({})", if platform::autostart_enabled() { "on" } else { "off" }, if ok { "ok" } else { "failed" });
                std::process::exit(if ok { 0 } else { 1 });
            }
            "--out" => c.clips_dir = PathBuf::from(&val),
            "--save-after" => c.save_after = val.parse().ok(),
            other => eprintln!("unknown arg {other}"),
        }
        i += 2;
    }
    c
}

/// `%APPDATA%\Bufo\ffmpeg.exe`: where the first-run download puts ffmpeg.
fn local_ffmpeg_path() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    settings::config_path()?.parent().map(|d| d.join(name))
}

#[cfg(windows)]
fn ffmpeg_works() -> bool {
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.arg("-version").stdout(Stdio::null()).stderr(Stdio::null());
    platform::no_window(&mut cmd);
    cmd.status().is_ok_and(|s| s.success())
}

/// The xz-compressed ffmpeg.exe embedded at build time (`--features bundle-ffmpeg`).
#[cfg(all(windows, feature = "bundle-ffmpeg"))]
static BUNDLED_FFMPEG_XZ: &[u8] = include_bytes!(env!("BUFO_FFMPEG_XZ"));

/// Unpack the embedded ffmpeg to `dest` (via a temp file, so a half-written copy is never used).
#[cfg(all(windows, feature = "bundle-ffmpeg"))]
fn unpack_bundled_ffmpeg(dest: &std::path::Path) -> bool {
    platform::notify("Bufo: first start", "Unpacking the video encoder (one time, a few seconds).");
    if let Some(d) = dest.parent() {
        let _ = fs::create_dir_all(d);
    }
    let tmp = dest.with_extension("part");
    let ok = fs::File::create(&tmp).is_ok_and(|f| {
        let mut out = std::io::BufWriter::new(f);
        lzma_rs::xz_decompress(&mut &BUNDLED_FFMPEG_XZ[..], &mut out).is_ok() && std::io::Write::flush(&mut out).is_ok()
    });
    let ok = ok && fs::rename(&tmp, dest).is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
    }
    settings::log(if ok { "bundled ffmpeg unpacked" } else { "bundled ffmpeg unpack failed" });
    ok
}

/// First-run setup: if no ffmpeg can be found, download the official "essentials"
/// build once (Windows 10/11 ship `curl` and `tar`) into `%APPDATA%\Bufo`.
/// Returns whether ffmpeg is usable afterwards.
#[cfg(windows)]
fn ensure_ffmpeg() -> bool {
    if ffmpeg_works() {
        return true;
    }
    let Some(dest) = local_ffmpeg_path() else { return false };
    // Single-file build: ffmpeg travels inside bufo.exe, no download needed.
    #[cfg(feature = "bundle-ffmpeg")]
    if unpack_bundled_ffmpeg(&dest) {
        return ffmpeg_works();
    }
    const URL: &str = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip";
    platform::notify("Bufo: first start", "Downloading ffmpeg (one time, about 120 MB). Recording starts when it is done.");
    settings::log("ffmpeg not found, downloading");
    let tmp = env::temp_dir().join("bufo-ffmpeg-setup");
    let _ = fs::remove_dir_all(&tmp);
    if fs::create_dir_all(&tmp).is_err() {
        return false;
    }
    let zip = tmp.join("ffmpeg.zip");
    let mut dl = Command::new("curl.exe");
    dl.args(["-L", "--fail", "--silent", "--retry", "3", "-o"]).arg(&zip).arg(URL);
    platform::no_window(&mut dl);
    let mut ok = dl.status().is_ok_and(|s| s.success());
    if ok {
        let mut x = Command::new("tar.exe");
        x.arg("-xf").arg(&zip).arg("-C").arg(&tmp);
        platform::no_window(&mut x);
        ok = x.status().is_ok_and(|s| s.success());
    }
    if ok {
        ok = false;
        for e in fs::read_dir(&tmp).into_iter().flatten().flatten() {
            let exe = e.path().join("bin").join("ffmpeg.exe");
            if exe.exists() {
                if let Some(d) = dest.parent() {
                    let _ = fs::create_dir_all(d);
                }
                ok = fs::copy(&exe, &dest).is_ok();
                break;
            }
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    settings::log(if ok { "ffmpeg downloaded" } else { "ffmpeg download failed" });
    if !ok {
        platform::notify("Bufo: ffmpeg download failed", "Check your internet, or put ffmpeg.exe next to bufo.exe.");
    }
    ok && ffmpeg_works()
}

#[cfg(not(windows))]
fn ensure_ffmpeg() -> bool {
    true
}

/// ffmpeg executable: `FFMPEG` env var, else `ffmpeg` from PATH.
/// Where ffmpeg is: `FFMPEG` env var, next to bufo.exe, a WinGet install, else PATH.
fn ffmpeg_bin() -> String {
    // The copy Bufo downloaded itself on first run wins over everything but FFMPEG.
    if env::var("FFMPEG").is_err() {
        if let Some(p) = local_ffmpeg_path().filter(|p| p.exists()) {
            return p.to_string_lossy().into_owned();
        }
    }
    static BIN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(p) = env::var("FFMPEG") {
            return p;
        }
        if let Some(p) = env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("ffmpeg.exe"))) {
            if p.exists() {
                return p.to_string_lossy().into_owned();
            }
        }
        // WinGet: Packages\Gyan.FFmpeg_*\ffmpeg-*\bin\ffmpeg.exe
        if let Ok(local) = env::var("LOCALAPPDATA") {
            let pk = PathBuf::from(local).join("Microsoft").join("WinGet").join("Packages");
            for e in fs::read_dir(pk).into_iter().flatten().flatten() {
                if !e.file_name().to_string_lossy().contains("FFmpeg") {
                    continue;
                }
                for sub in fs::read_dir(e.path()).into_iter().flatten().flatten() {
                    let p = sub.path().join("bin").join("ffmpeg.exe");
                    if p.exists() {
                        return p.to_string_lossy().into_owned();
                    }
                }
            }
        }
        "ffmpeg".to_string()
    })
    .clone()
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq)]
enum Capture {
    /// DXGI desktop duplication, frames stay on the GPU (fast path).
    Dda,
    /// GDI desktop grab, CPU copy (works everywhere, slower).
    Gdi,
}

#[cfg(windows)]
fn start_recorder(c: &Config, cap: Capture, audio: bool) -> std::io::Result<Child> {
    fs::create_dir_all(&c.buffer_dir)?;
    let pattern = c.buffer_dir.join("seg_%06d.ts");
    let gop = (c.fps as u64 * seg_secs()).to_string();
    let log = fs::File::create(c.buffer_dir.join("ffmpeg.log"))?;
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "warning", "-y"]);
    match cap {
        Capture::Dda => {
            cmd.args(["-f", "lavfi", "-i"])
                .arg(format!("ddagrab=output_idx=0:framerate={}", c.fps));
        }
        Capture::Gdi => {
            cmd.args(["-f", "gdigrab", "-framerate"])
                .arg(c.fps.to_string())
                .args(["-i", "desktop"]);
        }
    }
    if audio {
        // Raw PCM from our WASAPI thread arrives on stdin.
        cmd.args(["-f", "s16le", "-ar", "48000", "-ac", "2", "-i", "pipe:0", "-map", "0:v", "-map", "1:a"]);
    }
    let enc = chosen_encoder(c);
    let vf = if cap == Capture::Dda { enc.vf_dda } else { enc.vf_gdi };
    if !vf.is_empty() {
        cmd.args(["-vf", vf]);
    }
    cmd.args(&enc.args).args(["-g", &gop]);
    if audio {
        cmd.args(["-c:a", "aac", "-b:a", "160k"]);
    } else {
        cmd.arg("-an");
    }
    cmd
        .args(["-f", "segment", "-segment_time", &seg_secs().to_string()])
        .args(["-segment_format", "mpegts", "-reset_timestamps", "1"])
        .arg(pattern)
        .stdin(if audio { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
}

/// Feed the audio thread's PCM to ffmpeg's stdin, dropping the stale backlog
/// so audio and video start together (ffmpeg needs audio to begin at all).
#[cfg(windows)]
fn pipe_audio(child: &mut Child, rx: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<Vec<u8>>>>, dir: PathBuf) {
    use std::io::Write;
    let Some(mut stdin) = child.stdin.take() else { return };
    thread::spawn(move || {
        let Ok(rx) = rx.lock() else { return };
        while rx.try_recv().is_ok() {}
        // ffmpeg starts the audio clock at the first byte but the video clock only
        // when the encoder delivers its first frame (about D seconds later). Once the
        // first segment shows up, drop D seconds of audio so both clocks agree.
        let t0 = Instant::now();
        let mut skip: Option<usize> = None; // bytes still to drop
        while let Ok(chunk) = rx.recv() {
            if skip.is_none() && (!segments(&dir).is_empty() || t0.elapsed() > Duration::from_secs(5)) {
                let d = t0.elapsed().as_secs_f64();
                skip = Some((d * 48_000.0) as usize * 4);
            }
            if let Some(left) = skip.as_mut() {
                if *left > 0 {
                    *left = left.saturating_sub(chunk.len());
                    continue;
                }
            }
            if stdin.write_all(&chunk).is_err() {
                break;
            }
        }
    });
}

/// One hardware encoder that ffmpeg can drive on the Windows capture path.
#[cfg(windows)]
#[derive(Clone)]
struct Enc {
    name: &'static str,
    /// Video filter after ddagrab (GPU frames) / gdigrab (CPU frames).
    vf_dda: &'static str,
    vf_gdi: &'static str,
    args: Vec<String>,
}

/// NVIDIA, AMD and Intel hardware HEVC encoders, best first.
#[cfg(windows)]
fn encoder_candidates(c: &Config) -> Vec<Enc> {
    let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let br = format!("{}", c.bitrate);
    let mut nv = s(&["-c:v", "hevc_nvenc", "-preset", "p1", "-tune", "ull", "-delay", "0", "-surfaces", "4", "-rc", "vbr", "-cq"]);
    nv.extend(s(&[&c.cq.to_string(), "-b:v", "0", "-maxrate", "16M", "-bufsize", "32M", "-forced-idr", "1"]));
    let mut amf = s(&["-c:v", "hevc_amf", "-usage", "lowlatency", "-quality", "speed", "-rc", "vbr_latency", "-b:v"]);
    amf.extend(s(&[&br, "-maxrate", "16M"]));
    let mut qsv = s(&["-c:v", "hevc_qsv", "-preset", "veryfast", "-look_ahead", "0", "-b:v"]);
    qsv.extend(s(&[&br, "-maxrate", "16M"]));
    vec![
        Enc { name: "NVIDIA NVENC", vf_dda: "", vf_gdi: "", args: nv },
        Enc { name: "AMD AMF", vf_dda: "", vf_gdi: "", args: amf },
        Enc { name: "Intel Quick Sync", vf_dda: "hwmap=derive_device=qsv,format=qsv", vf_gdi: "format=nv12", args: qsv },
    ]
}

/// Does this encoder work on this machine, through the real capture chain?
#[cfg(windows)]
fn encoder_works(e: &Enc) -> bool {
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "ddagrab=output_idx=0:framerate=30"]);
    if !e.vf_dda.is_empty() {
        cmd.args(["-vf", e.vf_dda]);
    }
    cmd.args(["-frames:v", "5"]).args(&e.args).args(["-f", "null", "-"]).stdin(Stdio::null());
    platform::no_window(&mut cmd);
    cmd.output().is_ok_and(|o| o.status.success())
}

/// The encoder to use (probed once). `None` = none of them works.
#[cfg(windows)]
fn chosen_encoder_opt(c: &Config) -> Option<Enc> {
    static CHOSEN: std::sync::OnceLock<Option<Enc>> = std::sync::OnceLock::new();
    CHOSEN
        .get_or_init(|| {
            // A probe can fail once while the GPU is busy or a previous capture is
            // still shutting down, so try a few times before giving up.
            let mut found = None;
            for round in 0..3 {
                found = encoder_candidates(c).into_iter().find(encoder_works);
                if found.is_some() {
                    break;
                }
                if round < 2 {
                    thread::sleep(Duration::from_secs(1));
                }
            }
            match &found {
                Some(e) => settings::log(&format!("encoder: {}", e.name)),
                None => settings::log("encoder: no hardware encoder found through ffmpeg"),
            }
            found
        })
        .clone()
}

#[cfg(windows)]
fn chosen_encoder(c: &Config) -> Enc {
    chosen_encoder_opt(c).unwrap_or_else(|| encoder_candidates(c).remove(0))
}

/// Start with the GPU path; if no segment shows up in time, fall back to GDI.
#[cfg(windows)]
fn start_with_fallback(c: &Config, audio: Option<Receiver<Vec<u8>>>) -> std::io::Result<Child> {
    let has_audio = audio.is_some();
    let audio = audio.map(|r| Arc::new(std::sync::Mutex::new(r)));
    let mut child = start_recorder(c, Capture::Dda, has_audio)?;
    if let Some(rx) = &audio {
        pipe_audio(&mut child, Arc::clone(rx), c.buffer_dir.clone());
    }
    for _ in 0..12 {
        thread::sleep(Duration::from_millis(500));
        if !segments(&c.buffer_dir).is_empty() {
            println!("capture: ddagrab (GPU)");
            return Ok(child);
        }
    }
    eprintln!("ddagrab produced no frames, falling back to gdigrab");
    let _ = child.kill();
    let _ = child.wait();
    let mut child = start_recorder(c, Capture::Gdi, has_audio)?;
    if let Some(rx) = &audio {
        pipe_audio(&mut child, Arc::clone(rx), c.buffer_dir.clone());
    }
    println!("capture: gdigrab (CPU copy)");
    Ok(child)
}

/// Hardware encoder arguments for Linux: NVENC, then VAAPI, then software x264.
#[cfg(not(windows))]
fn linux_encoder(c: &Config) -> (Vec<String>, Vec<String>, &'static str) {
    let list = Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let br = c.bitrate.to_string();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    // Being listed only means ffmpeg was built with it; try a real 1-frame encode.
    let works = |pre: &[String], enc: &[String]| {
        Command::new(ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "quiet"])
            .args(pre)
            .args(["-f", "lavfi", "-i", "color=size=320x180:rate=1", "-frames:v", "1"])
            .args(enc)
            .args(["-f", "null", "-"])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if list.contains("hevc_nvenc") {
        let enc = s(&["-c:v", "hevc_nvenc", "-preset", "p4", "-b:v", &br, "-maxrate", "16M", "-bufsize", "32M"]);
        if works(&[], &enc) {
            return (vec![], enc, "NVENC");
        }
    }
    if list.contains("hevc_vaapi") && Path::new("/dev/dri/renderD128").exists() {
        let pre = s(&["-vaapi_device", "/dev/dri/renderD128"]);
        let enc = s(&["-vf", "format=nv12,hwupload", "-c:v", "hevc_vaapi", "-b:v", &br]);
        if works(&pre, &enc) {
            return (pre, enc, "VAAPI");
        }
    }
    (vec![], s(&["-c:v", "libx264", "-preset", "veryfast", "-b:v", &br]), "x264 (software)")
}

/// Linux: X11 screen grab through ffmpeg (Wayland needs the portal, not done yet).
#[cfg(not(windows))]
fn start_linux(c: &Config) -> Result<Child, String> {
    if env::var_os("DISPLAY").is_none() {
        return Err("no X11 display (Wayland capture is not implemented yet)".into());
    }
    fs::create_dir_all(&c.buffer_dir).map_err(|e| e.to_string())?;
    clean_buffer(&c.buffer_dir);
    let pattern = c.buffer_dir.join("seg_%06d.ts");
    let gop = (c.fps as u64 * seg_secs()).to_string();
    let log = fs::File::create(c.buffer_dir.join("ffmpeg.log")).map_err(|e| e.to_string())?;
    let (pre, enc, name) = linux_encoder(c);
    println!("encoder: {name}");
    let display = env::var("DISPLAY").unwrap_or_else(|_| ":0.0".into());
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "warning", "-y"]).args(&pre);
    cmd.args(["-f", "x11grab", "-framerate"]).arg(c.fps.to_string()).args(["-i", &display]);
    if c.audio {
        // Monitor of the default output = everything you hear.
        cmd.args(["-f", "pulse", "-i", "@DEFAULT_MONITOR@"]);
    }
    cmd.args(&enc).args(["-g", &gop, "-force_key_frames", &format!("expr:gte(t,n_forced*{})", seg_secs())]);
    if c.audio {
        cmd.args(["-c:a", "aac", "-b:a", "160k"]);
    } else {
        cmd.arg("-an");
    }
    cmd.args(["-f", "segment", "-segment_time", &seg_secs().to_string()])
        .args(["-segment_format", "mpegts", "-reset_timestamps", "1"])
        .arg(pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|e| e.to_string())
}

/// Sorted list of segment files (oldest first).
fn segments(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "ts" || x == "mp4"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Keep only the newest `buffer_secs` worth of segments.
fn prune_loop(dir: PathBuf, buffer_secs: u64) {
    let keep = (buffer_secs / seg_secs() + 2) as usize;
    loop {
        thread::sleep(Duration::from_secs(seg_secs()));
        mictrack::prune(&dir, mictrack::unix_ms().saturating_sub((buffer_secs + 3 * seg_secs()) * 1000));
        let segs = segments(&dir);
        if segs.len() > keep {
            for old in &segs[..segs.len() - keep] {
                let _ = fs::remove_file(old);
            }
        }
    }
}

/// Concatenate the newest segments into an mp4. Returns the file and its
/// approximate length in seconds.
fn save_clip(c: &Config, secs: u64) -> Option<(PathBuf, u64)> {
    let mut segs = segments(&c.buffer_dir);
    // ffmpeg's newest .ts is still being written; wgc only publishes finished .mp4.
    if segs.last().is_some_and(|p| p.extension().is_some_and(|x| x == "ts")) {
        segs.pop();
    }
    if segs.is_empty() {
        eprintln!("buffer is empty, nothing to save");
        return None;
    }
    // Round up so the clip is never shorter than requested.
    let want = secs.div_ceil(seg_secs()).max(1) as usize;
    let start = segs.len().saturating_sub(want);
    let chosen = &segs[start..];

    fs::create_dir_all(&c.clips_dir).ok()?;
    let list = c.buffer_dir.join("concat.txt");
    let body: String = chosen
        .iter()
        .map(|p| format!("file '{}'\n", p.to_string_lossy().replace('\\', "/")))
        .collect();
    fs::write(&list, body).ok()?;

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let out = c.clips_dir.join(format!("clip_{stamp}.mp4"));
    let status = Command::new(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(&out)
        .status()
        .map_err(|e| settings::log(&format!("cannot run ffmpeg: {e}")))
        .ok()?;
    if status.success() {
        if c.audio && c.mic && c.mic_track {
            let end_ms = chosen
                .last()
                .and_then(|p| fs::metadata(p).ok()?.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64);
            let dur = export::duration(&out);
            if let (Some(end_ms), Some(dur)) = (end_ms, dur) {
                if !mictrack::add_track(&ffmpeg_bin(), &c.buffer_dir, &out, end_ms, dur) {
                    settings::log("microphone track could not be added");
                }
            }
        }
        println!("saved {} ({} segments)", out.display(), chosen.len());
        Some((out, chosen.len() as u64 * seg_secs()))
    } else {
        eprintln!("ffmpeg concat failed");
        None
    }
}

/// Remove every leftover segment (finished or half-written) from the buffer.
fn clean_buffer(dir: &Path) {
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("seg_")) {
                let _ = fs::remove_file(p);
            }
        }
    }
}

enum Rec {
    Ffmpeg(Child),
    #[cfg(windows)]
    Wgc(wgc::Control),
    #[cfg(windows)]
    Dda(dda::Control),
}

impl Rec {
    fn stop(self) {
        match self {
            Rec::Ffmpeg(mut c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            #[cfg(windows)]
            Rec::Dda(c) => c.stop(),
            #[cfg(windows)]
            Rec::Wgc(c) => {
                let _ = c.stop();
            }
        }
    }
}

#[cfg(windows)]
fn start_wgc(c: &Config) -> Result<Rec, String> {
    fs::create_dir_all(&c.buffer_dir).map_err(|e| e.to_string())?;
    clean_buffer(&c.buffer_dir);
    // WGC only delivers frames when the picture changes, so a static desktop
    // legitimately produces nothing; a successful start is enough.
    let audio_rx = if c.audio { audio::start(c.mic, c.mic_track.then(|| c.buffer_dir.clone())) } else { None };
    if audio_rx.is_none() {
        println!("audio: off");
    }
    let ctl = wgc::start(c.buffer_dir.clone(), c.fps, c.bitrate, seg_secs(), audio_rx)
        .map_err(|e| e.to_string())?;
    Ok(Rec::Wgc(ctl))
}

#[cfg(windows)]
fn start_dda(c: &Config) -> Result<Rec, String> {
    fs::create_dir_all(&c.buffer_dir).map_err(|e| e.to_string())?;
    clean_buffer(&c.buffer_dir);
    let audio_rx = if c.audio { audio::start(c.mic, c.mic_track.then(|| c.buffer_dir.clone())) } else { None };
    if audio_rx.is_none() {
        println!("audio: off");
    }
    let ctl = dda::start(c.buffer_dir.clone(), c.fps, c.bitrate, seg_secs(), audio_rx)?;
    Ok(Rec::Dda(ctl))
}

#[cfg(windows)]
fn start_capture(c: &Config) -> Option<Rec> {
    let mut c = c.clone();
    if c.capture == "auto" {
        c.capture = if chosen_encoder_opt(&c).is_some() { "ffmpeg" } else { "dda" }.to_string();
    }
    let c = &c;
    if c.capture == "dda" {
        match start_dda(c) {
            Ok(r) => {
                println!("capture: Desktop Duplication -> HEVC encoder");
                return Some(r);
            }
            Err(e) => eprintln!("desktop duplication failed ({e}), trying Windows Graphics Capture"),
        }
    }
    if c.capture == "wgc" || c.capture == "dda" {
        match start_wgc(c) {
            Ok(r) => {
                println!("capture: Windows Graphics Capture -> HEVC encoder");
                return Some(r);
            }
            Err(e) => eprintln!("wgc failed ({e}), falling back to ffmpeg"),
        }
    }
    clean_buffer(&c.buffer_dir);
    let audio_rx = if c.audio { audio::start(c.mic, c.mic_track.then(|| c.buffer_dir.clone())) } else { None };
    match start_with_fallback(c, audio_rx) {
        Ok(ch) => Some(Rec::Ffmpeg(ch)),
        Err(e) => {
            eprintln!("failed to start ffmpeg (is it in PATH?): {e}");
            None
        }
    }
}

/// 32x32 red "recording" dot with a white ring, generated in code.
#[cfg(windows)]
fn make_icon() -> tray_icon::Icon {
    let n = 32u32;
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let d = (dx * dx + dy * dy).sqrt();
            let px: [u8; 4] = if d < 11.0 {
                [230, 40, 40, 255]
            } else if d < 14.0 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&px);
        }
    }
    tray_icon::Icon::from_rgba(rgba, n, n).expect("valid icon")
}

/// Delete old clips according to `keep_days` / `max_clips_gb`. Returns how many
/// were removed. Only touches files named `clip_*.mp4` in the clips folder, and
/// never the newest one (so a clip you just saved cannot vanish).
fn cleanup_clips(cfg: &Config) -> usize {
    if cfg.max_clips_gb <= 0.0 && cfg.keep_days == 0 {
        return 0;
    }
    let Ok(rd) = fs::read_dir(&cfg.clips_dir) else { return 0 };
    // (path, modified, size), oldest first
    let mut clips: Vec<(PathBuf, SystemTime, u64)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("clip_") && n.ends_with(".mp4")
        })
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.modified().ok()?, m.len()))
        })
        .collect();
    clips.sort_by_key(|c| c.1);
    if clips.len() < 2 {
        return 0; // nothing but (at most) the newest clip: never delete it
    }
    let newest = clips.len() - 1;
    let mut removed = 0;
    let mut total: u64 = clips.iter().map(|c| c.2).sum();
    let limit = (cfg.max_clips_gb * 1_073_741_824.0) as u64;
    let max_age = Duration::from_secs(cfg.keep_days * 86_400);
    let now = SystemTime::now();
    for (i, (path, modified, size)) in clips.iter().enumerate() {
        if i == newest {
            break;
        }
        if gallery::is_favorite(path) {
            continue; // starred clips are kept
        }
        let too_old = cfg.keep_days > 0 && now.duration_since(*modified).is_ok_and(|a| a > max_age);
        let too_big = cfg.max_clips_gb > 0.0 && total > limit;
        if !(too_old || too_big) {
            // Sorted oldest first: once the oldest is fine by age, only size can still apply.
            if cfg.max_clips_gb <= 0.0 {
                break;
            }
            continue;
        }
        if fs::remove_file(path).is_ok() {
            settings::log(&format!("auto-deleted {} ({} MB)", path.display(), size / 1_048_576));
            total = total.saturating_sub(*size);
            removed += 1;
        }
    }
    removed
}

/// Grab the whole screen once into `shot_<time>.png` in the clips folder.
fn take_screenshot(cfg: &Config) {
    let _ = fs::create_dir_all(&cfg.clips_dir);
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = cfg.clips_dir.join(format!("shot_{secs}.png"));
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    #[cfg(windows)]
    cmd.args(["-f", "gdigrab", "-framerate", "1", "-i", "desktop"]);
    #[cfg(not(windows))]
    cmd.args(["-f", "x11grab", "-i", &env::var("DISPLAY").unwrap_or_else(|_| ":0.0".into())]);
    cmd.args(["-frames:v", "1"]).arg(&path).stdin(Stdio::null());
    platform::no_window(&mut cmd);
    if cmd.status().is_ok_and(|s| s.success()) && path.exists() {
        settings::log(&format!("screenshot {}", path.display()));
        if cfg.clipboard {
            copy_to_clipboard(&path);
        }
        platform::beep();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        platform::notify("Bufo: screenshot saved", &name);
    } else {
        settings::log("screenshot failed (ffmpeg error)");
    }
}

/// Save a clip and give an audible confirmation.
fn save_with_feedback(cfg: &Config, secs: u64) {
    match save_clip(cfg, secs) {
        Some((path, duration)) => {
            settings::log(&format!("saved {}", path.display()));
            gallery::register(cfg, &path, &winfo::last_app(), duration);
            cleanup_clips(cfg);
            if cfg.clipboard {
                copy_to_clipboard(&path);
            }
            platform::beep();
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let extra = if cfg.clipboard { " Copied to clipboard." } else { "" };
            platform::notify("Bufo: clip saved", &format!("{name}.{extra}"));
        }
        None => settings::log("save failed (buffer empty or ffmpeg error)"),
    }
}

/// Put the clip file on the clipboard as a file (Ctrl+V pastes it into chats).
/// Done on a short-lived thread so the hotkey loop never blocks.
fn copy_to_clipboard(path: &std::path::Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let ok = platform::copy_file_to_clipboard(&path);
        settings::log(if ok { "clip copied to clipboard" } else { "clipboard copy failed" });
    });
}

/// Only one recorder may run: two would fight over the buffer folder.
fn already_running() -> bool {
    platform::claim_instance("BufoSingleInstance")
}

/// Linux: capture through ffmpeg (X11). Windows has its own native path above.
#[cfg(not(windows))]
fn start_capture(c: &Config) -> Option<Rec> {
    match start_linux(c) {
        Ok(ch) => Some(Rec::Ffmpeg(ch)),
        Err(e) => {
            eprintln!("capture failed: {e}");
            None
        }
    }
}

/// Linux: global hotkeys on X11 (no tray yet); `save` / `quit` typed in the
/// terminal also work.
#[cfg(not(windows))]
fn run_interactive(cfg: &Arc<Config>) {
    use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use std::{io::BufRead, str::FromStr, sync::mpsc};

    #[derive(Clone, Copy)]
    enum Cmd {
        Save,
        Long,
        Shot,
        Quit,
    }
    // global-hotkey wants "super", not "win".
    let parse = |text: &str| HotKey::from_str(&text.to_ascii_lowercase().replace("win", "super")).ok();
    let (save_hk, quit_hk, shot_hk, long_hk) =
        (parse(&cfg.hotkey_save), parse(&cfg.hotkey_quit), parse(&cfg.hotkey_shot), parse(&cfg.hotkey_long));

    // Keep the manager alive for as long as we listen.
    let manager = GlobalHotKeyManager::new()
        .map_err(|e| eprintln!("global hotkeys unavailable: {e} (X11 only; use the terminal)"))
        .ok();
    if let Some(m) = &manager {
        for (name, hk) in [("save", save_hk), ("quit", quit_hk), ("screenshot", shot_hk), ("long clip", long_hk)] {
            match hk {
                Some(hk) => {
                    if let Err(e) = m.register(hk) {
                        eprintln!("hotkey for {name} not registered: {e}");
                    }
                }
                None => eprintln!("hotkey for {name} not understood"),
            }
        }
    }

    let (tx, rx) = mpsc::channel::<Cmd>();
    {
        let tx = tx.clone();
        let (sid, qid, pid, lid) =
            (save_hk.map(|h| h.id()), quit_hk.map(|h| h.id()), shot_hk.map(|h| h.id()), long_hk.map(|h| h.id()));
        thread::spawn(move || {
            for ev in GlobalHotKeyEvent::receiver() {
                if ev.state != HotKeyState::Pressed {
                    continue;
                }
                let cmd = if Some(ev.id) == sid {
                    Cmd::Save
                } else if Some(ev.id) == qid {
                    Cmd::Quit
                } else if Some(ev.id) == lid {
                    Cmd::Long
                } else if Some(ev.id) == pid {
                    Cmd::Shot
                } else {
                    continue;
                };
                if tx.send(cmd).is_err() {
                    break;
                }
            }
        });
    }
    // Terminal control; when stdin is closed (launched from a menu) this just ends.
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            let cmd = match line.trim() {
                "save" | "s" => Cmd::Save,
                "long" | "l" => Cmd::Long,
                "shot" | "screenshot" => Cmd::Shot,
                "quit" | "q" => Cmd::Quit,
                _ => continue,
            };
            if tx.send(cmd).is_err() {
                break;
            }
        }
    });
    println!("type 'save' to save a clip, 'quit' to exit");
    for cmd in rx {
        match cmd {
            Cmd::Save => save_with_feedback(cfg, cfg.clip_secs),
            Cmd::Long => save_with_feedback(cfg, cfg.clip_long_secs),
            Cmd::Shot => take_screenshot(cfg),
            Cmd::Quit => break,
        }
    }
    drop(manager);
}

fn main() {
    // No settings file yet = the very first start: also open the window for the user.
    let first_run = settings::config_path().is_some_and(|p| !p.exists());
    let cfg = Arc::new(parse_args());
    if cfg.ui {
        if !platform::claim_instance("BufoUiInstance") {
            ui::run((*cfg).clone());
        }
        return;
    }
    if cfg.cleanup_only {
        println!("cleanup: removed {} clip(s)", cleanup_clips(&cfg));
        return;
    }
    if cfg.shot_only {
        take_screenshot(&cfg);
        thread::sleep(Duration::from_secs(2)); // let the clipboard copy finish
        return;
    }
    if let Some((kind, target)) = &cfg.edit {
        let input = if target == "last" { export::newest_clip(&cfg) } else { Some(PathBuf::from(target)) };
        let (from, to) = (cfg.export_from.unwrap_or(0.0), cfg.export_to);
        let out = input.and_then(|i| {
            let to = to.or_else(|| export::duration(&i)).unwrap_or(0.0);
            if *kind == "trim" { export::trim(&i, from, to) } else { export::gif(&i, from, to.min(from + 15.0)) }
        });
        match out {
            Some(p) => println!("{kind}: {}", p.display()),
            None => eprintln!("{kind} failed"),
        }
        return;
    }
    if let Some(target) = &cfg.export {
        let input = if target == "last" { export::newest_clip(&cfg) } else { Some(PathBuf::from(target)) };
        match input.and_then(|i| {
            export::vertical(&i, cfg.export_from, cfg.export_to, export::Style::parse(&cfg.export_style))
        }) {
            Some(p) => println!("exported: {}", p.display()),
            None => eprintln!("export failed"),
        }
        return;
    }
    if cfg.gallery {
        match gallery::build(&cfg) {
            Some(p) => {
                println!("gallery: {}", p.display());
                if cfg.gallery_open {
                    gallery::open(&p);
                }
            }
            None => eprintln!("gallery: could not build"),
        }
        return;
    }
    if cfg.save_after.is_none() && already_running() {
        eprintln!("Bufo is already running");
        return;
    }
    if !ensure_ffmpeg() {
        eprintln!("ffmpeg is not available");
        return;
    }
    if first_run {
        if let Ok(exe) = env::current_exe() {
            let _ = Command::new(exe).arg("--ui").spawn();
        }
    }
    let child = match start_capture(&cfg) {
        Some(r) => Arc::new(Mutex::new(Some(r))),
        None => return,
    };
    println!(
        "recording: buffer {}s, clip {}s, {} fps. {} save, {} quit",
        cfg.buffer_secs, cfg.clip_secs, cfg.fps, cfg.hotkey_save, cfg.hotkey_quit
    );

    {
        let (dir, secs) = (cfg.buffer_dir.clone(), cfg.buffer_secs);
        thread::spawn(move || prune_loop(dir, secs));
    }

    let stop = {
        let child = Arc::clone(&child);
        move || {
            if let Ok(mut g) = child.lock() {
                if let Some(r) = g.take() {
                    r.stop();
                }
            }
        }
    };

    if let Some(secs) = cfg.save_after {
        thread::sleep(Duration::from_secs(secs));
        save_with_feedback(&cfg, cfg.clip_secs);
        thread::sleep(Duration::from_secs(3)); // let the clipboard copy finish
        stop();
        return;
    }

    run_interactive(&cfg);
    stop();
}

/// Tray icon, hotkeys and the Windows message loop; returns when the user quits.
#[cfg(windows)]
fn run_interactive(cfg: &Arc<Config>) {
    // Tray icon + menu. Its window lives on this thread, so the message loop
    // below must dispatch messages for it.
    let save_item = MenuItem::new(format!("Save clip ({})", cfg.hotkey_save), true, None);
    let app_item = MenuItem::new("Open Bufo", true, None);
    let shot_item = MenuItem::new(format!("Screenshot ({})", cfg.hotkey_shot), true, None);
    let gallery_item = MenuItem::new("Gallery in browser", true, None);
    let export_item = MenuItem::new("Export last clip as 9:16", true, None);
    let open_item = MenuItem::new("Open clips folder", true, None);
    let quit_item = MenuItem::new(format!("Quit ({})", cfg.hotkey_quit), true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &app_item,
        &save_item,
        &shot_item,
        &gallery_item,
        &export_item,
        &open_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ]);
    winfo::start_tracker();
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Bufo: recording")
        .with_icon(make_icon())
        .build()
        .map_err(|e| eprintln!("tray icon unavailable: {e}"))
        .ok();

    let main_thread = unsafe { GetCurrentThreadId() };
    {
        let cfg = Arc::clone(&cfg);
        let shot_id = shot_item.id().clone();
        let (app_id, save_id, gallery_id, export_id, open_id, quit_id) = (
            app_item.id().clone(),
            save_item.id().clone(),
            gallery_item.id().clone(),
            export_item.id().clone(),
            open_item.id().clone(),
            quit_item.id().clone(),
        );
        thread::spawn(move || {
            while let Ok(ev) = MenuEvent::receiver().recv() {
                if ev.id == app_id {
                    if let Ok(exe) = env::current_exe() {
                        let _ = Command::new(exe).arg("--ui").spawn();
                    }
                } else if ev.id == save_id {
                    save_with_feedback(&cfg, cfg.clip_secs);
                } else if ev.id == shot_id {
                    take_screenshot(&cfg);
                } else if ev.id == gallery_id {
                    if let Some(p) = gallery::build(&cfg) {
                        gallery::open(&p);
                    }
                } else if ev.id == export_id {
                    let made = export::newest_clip(&cfg)
                        .and_then(|i| export::vertical(&i, None, None, export::Style::Blur));
                    match made {
                        Some(p) => {
                            settings::log(&format!("exported {}", p.display()));
                            platform::beep();
                        }
                        None => settings::log("export failed (no clip yet, or ffmpeg error)"),
                    }
                } else if ev.id == open_id {
                    let _ = fs::create_dir_all(&cfg.clips_dir);
                    platform::open(&cfg.clips_dir);
                } else if ev.id == quit_id {
                    unsafe {
                        PostThreadMessageW(main_thread, WM_QUIT, 0, 0);
                    }
                }
            }
        });
    }

    unsafe {
        // A failed registration means another app already owns the combination
        // (e.g. NVIDIA's overlay owns Alt+F10); say so instead of failing silently.
        for (id, text) in [(1, &cfg.hotkey_save), (2, &cfg.hotkey_quit), (3, &cfg.hotkey_shot), (4, &cfg.hotkey_long)] {
            match settings::parse_hotkey(text) {
                Some((mods, vk)) => {
                    if RegisterHotKey(std::ptr::null_mut(), id, mods | MOD_NOREPEAT, vk) == 0 {
                        settings::log(&format!(
                            "hotkey {text} could not be registered (in use by another app?), error {}",
                            GetLastError()
                        ));
                    }
                }
                None => settings::log(&format!("hotkey '{text}' is not valid")),
            }
        }
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_HOTKEY {
                match msg.wParam {
                    1 => {
                        let cfg = Arc::clone(&cfg);
                        thread::spawn(move || save_with_feedback(&cfg, cfg.clip_secs));
                    }
                    4 => {
                        let cfg = Arc::clone(&cfg);
                        thread::spawn(move || save_with_feedback(&cfg, cfg.clip_long_secs));
                    }
                    2 => break,
                    3 => {
                        let cfg = Arc::clone(&cfg);
                        thread::spawn(move || take_screenshot(&cfg));
                    }
                    _ => {}
                }
            } else {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
