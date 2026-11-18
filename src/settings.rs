//! User settings file: `%APPDATA%\ClipRecorder\config.toml`.
//!
//! Precedence: built-in defaults < config file < command-line flags.
//! The file is created with the defaults on first run so it can be edited.

use std::{env, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FileConfig {
    /// Where saved clips go.
    pub clips_dir: Option<String>,
    /// Length of the rolling buffer, seconds.
    pub buffer_secs: Option<u64>,
    /// Length of a saved clip, seconds.
    pub clip_secs: Option<u64>,
    pub fps: Option<u32>,
    /// Video bitrate, bits per second.
    pub bitrate: Option<u32>,
    /// Segment length, seconds.
    pub segment_secs: Option<u64>,
    /// Record system audio.
    pub audio: Option<bool>,
    /// Mix the default microphone into the audio track.
    pub mic: Option<bool>,
    /// Hotkey that saves a clip, e.g. "Ctrl+Alt+F10".
    pub hotkey_save: Option<String>,
    /// Hotkey that quits the recorder.
    pub hotkey_quit: Option<String>,
}

/// Parse "Ctrl+Alt+F10" into (modifier flags, virtual-key code).
/// Modifiers: Ctrl, Alt, Shift, Win. Keys: F1-F24, A-Z, 0-9.
pub fn parse_hotkey(text: &str) -> Option<(u32, u32)> {
    let (mut mods, mut vk) = (0u32, None);
    for part in text.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        match part.as_str() {
            "alt" => mods |= 0x1,
            "ctrl" | "control" => mods |= 0x2,
            "shift" => mods |= 0x4,
            "win" => mods |= 0x8,
            k if k.len() > 1 && k.starts_with('f') => {
                let n: u32 = k[1..].parse().ok()?;
                if !(1..=24).contains(&n) {
                    return None;
                }
                vk = Some(0x70 + n - 1);
            }
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => {
                vk = Some(k.to_ascii_uppercase().as_bytes()[0] as u32);
            }
            _ => return None,
        }
    }
    vk.map(|v| (mods, v))
}

/// Append a line to `%APPDATA%\ClipRecorder\log.txt` (the app has no console).
pub fn log(msg: &str) {
    use std::io::Write;
    if let Some(dir) = config_path().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        let _ = fs::create_dir_all(&dir);
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")) {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(f, "[{secs}] {msg}");
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    Some(PathBuf::from(env::var("APPDATA").ok()?).join("ClipRecorder").join("config.toml"))
}

const TEMPLATE: &str = "\
# ClipRecorder settings. Delete a line to use the default.
# Command-line flags override this file.

# clips_dir = \"C:\\\\Users\\\\you\\\\Videos\\\\ClipRecorder\"
buffer_secs = 900     # rolling buffer length
clip_secs = 60        # length of a saved clip
fps = 60
bitrate = 6000000     # video bits per second
segment_secs = 8      # buffer segment length (longer = fewer encoder restarts)
audio = true          # system audio
mic = false           # mix the microphone into the audio track
hotkey_save = \"Ctrl+Alt+F10\"   # Alt+F10 is taken by NVIDIA's overlay
hotkey_quit = \"Ctrl+Alt+F11\"
";

/// Load the settings file, creating it from the template on first run.
pub fn load() -> FileConfig {
    let Some(path) = config_path() else { return FileConfig::default() };
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("config.toml ignored ({e})");
            FileConfig::default()
        }),
        Err(_) => {
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(&path, TEMPLATE);
            FileConfig::default()
        }
    }
}
