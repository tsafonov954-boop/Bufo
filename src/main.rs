//! ClipRecorder.
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

mod audio;
mod settings;
mod wgc;

use windows_sys::Win32::{
    Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
    System::{
        Diagnostics::Debug::MessageBeep,
        Threading::{CreateMutexW, GetCurrentThreadId},
    },
    UI::{
        Input::KeyboardAndMouse::{RegisterHotKey, MOD_NOREPEAT},
        WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MB_OK, MSG,
            WM_HOTKEY, WM_QUIT,
        },
    },
};
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
    hotkey_save: String,
    hotkey_quit: String,
    /// "wgc" (native, default) or "ffmpeg" (fallback path).
    capture: String,
    /// Test mode: save one clip after N seconds, then exit.
    save_after: Option<u64>,
}

fn parse_args() -> Config {
    let base = env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("Videos")
        .join("ClipRecorder");
    let mut c = Config {
        buffer_dir: env::temp_dir().join("cliprecorder-buffer"),
        clips_dir: base,
        buffer_secs: 900,
        clip_secs: 60,
        fps: 60,
        cq: 27,
        bitrate: 6_000_000,
        audio: true,
        mic: false,
        hotkey_save: "Ctrl+Alt+F10".to_string(),
        hotkey_quit: "Ctrl+Alt+F11".to_string(),
        capture: "wgc".to_string(),
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
    c.hotkey_save = f.hotkey_save.unwrap_or(c.hotkey_save);
    c.hotkey_quit = f.hotkey_quit.unwrap_or(c.hotkey_quit);
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
            "--out" => c.clips_dir = PathBuf::from(&val),
            "--save-after" => c.save_after = val.parse().ok(),
            other => eprintln!("unknown arg {other}"),
        }
        i += 2;
    }
    c
}

/// ffmpeg executable: `FFMPEG` env var, else `ffmpeg` from PATH.
fn ffmpeg_bin() -> String {
    env::var("FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string())
}

#[derive(Clone, Copy, PartialEq)]
enum Capture {
    /// DXGI desktop duplication, frames stay on the GPU (fast path).
    Dda,
    /// GDI desktop grab, CPU copy (works everywhere, slower).
    Gdi,
}

fn start_recorder(c: &Config, cap: Capture) -> std::io::Result<Child> {
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
    cmd.args(["-c:v", "hevc_nvenc", "-preset", "p4", "-rc", "vbr"])
        .args(["-cq", &c.cq.to_string(), "-b:v", "0", "-maxrate", "16M", "-bufsize", "32M"])
        .args(["-g", &gop, "-forced-idr", "1", "-an"])
        .args(["-f", "segment", "-segment_time", &seg_secs().to_string()])
        .args(["-segment_format", "mpegts", "-reset_timestamps", "1"])
        .arg(pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
}

/// Start with the GPU path; if no segment shows up in time, fall back to GDI.
fn start_with_fallback(c: &Config) -> std::io::Result<Child> {
    let mut child = start_recorder(c, Capture::Dda)?;
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
    let child = start_recorder(c, Capture::Gdi)?;
    println!("capture: gdigrab (CPU copy)");
    Ok(child)
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
        let segs = segments(&dir);
        if segs.len() > keep {
            for old in &segs[..segs.len() - keep] {
                let _ = fs::remove_file(old);
            }
        }
    }
}

fn save_clip(c: &Config) -> Option<PathBuf> {
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
    let want = c.clip_secs.div_ceil(seg_secs()).max(1) as usize;
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
        .ok()?;
    if status.success() {
        println!("saved {} ({} segments)", out.display(), chosen.len());
        Some(out)
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
    Wgc(wgc::Control),
}

impl Rec {
    fn stop(self) {
        match self {
            Rec::Ffmpeg(mut c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            Rec::Wgc(c) => {
                let _ = c.stop();
            }
        }
    }
}

fn start_wgc(c: &Config) -> Result<Rec, String> {
    fs::create_dir_all(&c.buffer_dir).map_err(|e| e.to_string())?;
    clean_buffer(&c.buffer_dir);
    // WGC only delivers frames when the picture changes, so a static desktop
    // legitimately produces nothing; a successful start is enough.
    let audio_rx = if c.audio { audio::start(c.mic) } else { None };
    if audio_rx.is_none() {
        println!("audio: off");
    }
    let ctl = wgc::start(c.buffer_dir.clone(), c.fps, c.bitrate, seg_secs(), audio_rx)
        .map_err(|e| e.to_string())?;
    Ok(Rec::Wgc(ctl))
}

fn start_capture(c: &Config) -> Option<Rec> {
    if c.capture == "wgc" {
        match start_wgc(c) {
            Ok(r) => {
                println!("capture: Windows Graphics Capture -> HEVC encoder");
                return Some(r);
            }
            Err(e) => eprintln!("wgc failed ({e}), falling back to ffmpeg"),
        }
    }
    clean_buffer(&c.buffer_dir);
    match start_with_fallback(c) {
        Ok(ch) => Some(Rec::Ffmpeg(ch)),
        Err(e) => {
            eprintln!("failed to start ffmpeg (is it in PATH?): {e}");
            None
        }
    }
}

/// 32x32 red "recording" dot with a white ring, generated in code.
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

/// Save a clip and give an audible confirmation.
fn save_with_feedback(cfg: &Config) {
    match save_clip(cfg) {
        Some(path) => {
            settings::log(&format!("saved {}", path.display()));
            unsafe {
                MessageBeep(MB_OK);
            }
        }
        None => settings::log("save failed (buffer empty or ffmpeg error)"),
    }
}

/// Only one recorder may run: two would fight over the buffer folder.
fn already_running() -> bool {
    let name: Vec<u16> = "Local\\ClipRecorderSingleInstance\0".encode_utf16().collect();
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        // The handle is intentionally kept open for the life of the process.
        handle.is_null() || GetLastError() == ERROR_ALREADY_EXISTS
    }
}

fn main() {
    let cfg = Arc::new(parse_args());
    if cfg.save_after.is_none() && already_running() {
        eprintln!("ClipRecorder is already running");
        return;
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
        save_clip(&cfg);
        stop();
        return;
    }

    // Tray icon + menu. Its window lives on this thread, so the message loop
    // below must dispatch messages for it.
    let save_item = MenuItem::new(format!("Save clip ({})", cfg.hotkey_save), true, None);
    let open_item = MenuItem::new("Open clips folder", true, None);
    let quit_item = MenuItem::new(format!("Quit ({})", cfg.hotkey_quit), true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&save_item, &open_item, &PredefinedMenuItem::separator(), &quit_item]);
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("ClipRecorder: recording")
        .with_icon(make_icon())
        .build()
        .map_err(|e| eprintln!("tray icon unavailable: {e}"))
        .ok();

    let main_thread = unsafe { GetCurrentThreadId() };
    {
        let cfg = Arc::clone(&cfg);
        let (save_id, open_id, quit_id) =
            (save_item.id().clone(), open_item.id().clone(), quit_item.id().clone());
        thread::spawn(move || {
            while let Ok(ev) = MenuEvent::receiver().recv() {
                if ev.id == save_id {
                    save_with_feedback(&cfg);
                } else if ev.id == open_id {
                    let _ = fs::create_dir_all(&cfg.clips_dir);
                    let _ = Command::new("explorer").arg(&cfg.clips_dir).spawn();
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
        for (id, text) in [(1, &cfg.hotkey_save), (2, &cfg.hotkey_quit)] {
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
                        thread::spawn(move || save_with_feedback(&cfg));
                    }
                    2 => break,
                    _ => {}
                }
            } else {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    stop();
}
