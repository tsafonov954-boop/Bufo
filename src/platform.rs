//! Everything that differs between operating systems, behind one small API.

use std::{path::Path, process::Command};

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS},
        System::{
            Diagnostics::Debug::MessageBeep,
            Threading::{CreateMutexW, OpenMutexW},
            Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION},
        },
        UI::WindowsAndMessaging::MB_OK,
    };

    const SYNCHRONIZE: u32 = 0x0010_0000;

    pub fn beep() {
        unsafe {
            MessageBeep(MB_OK);
        }
    }

    /// Open a file or folder with the default program.
    pub fn open(path: &Path) {
        let _ = Command::new("explorer").arg(path).spawn();
    }

    /// Show a file selected in the file manager.
    pub fn reveal(path: &Path) {
        let _ = Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }

    /// Create the named mutex; true if someone else already owns it. The handle
    /// stays open for the life of the process on purpose.
    pub fn claim_instance(name: &str) -> bool {
        let name: Vec<u16> = format!("Local\\{name}\0").encode_utf16().collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            handle.is_null() || GetLastError() == ERROR_ALREADY_EXISTS
        }
    }

    /// Does a process currently hold this instance name?
    pub fn instance_running(name: &str) -> bool {
        let name: Vec<u16> = format!("Local\\{name}\0").encode_utf16().collect();
        unsafe {
            let h = OpenMutexW(SYNCHRONIZE, 0, name.as_ptr());
            if h.is_null() {
                false
            } else {
                CloseHandle(h);
                true
            }
        }
    }

    /// Local time zone offset from UTC in seconds (includes daylight saving).
    pub fn local_offset_secs() -> i64 {
        unsafe {
            let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
            let kind = GetTimeZoneInformation(&mut tz);
            let extra = if kind == 2 { tz.DaylightBias } else { tz.StandardBias };
            -((tz.Bias + extra) as i64) * 60
        }
    }

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    fn reg(args: &[&str]) -> std::process::Output {
        Command::new("reg")
            .args(args)
            .creation_flags(0x0800_0000)
            .output()
            .unwrap_or_else(|_| std::process::Output {
                status: std::process::ExitStatus::default(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
    }

    /// Is Bufo registered to start when the user logs in?
    pub fn autostart_enabled() -> bool {
        reg(&["query", RUN_KEY, "/v", "Bufo"]).status.success()
    }

    /// Add or remove the per-user "run at login" entry (no admin needed).
    pub fn set_autostart(on: bool) -> bool {
        if on {
            let Ok(exe) = std::env::current_exe() else { return false };
            let cmd = format!("\"{}\"", exe.display());
            reg(&["add", RUN_KEY, "/v", "Bufo", "/t", "REG_SZ", "/d", &cmd, "/f"]).status.success()
        } else {
            !autostart_enabled() || reg(&["delete", RUN_KEY, "/v", "Bufo", "/f"]).status.success()
        }
    }

    /// Put a file on the clipboard (Ctrl+V pastes it into chats).
    pub fn copy_file_to_clipboard(path: &Path) -> bool {
        let p = std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().trim_start_matches(r"\\?\").to_string())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", "Set-Clipboard -LiteralPath $env:BUFO_CLIP"])
            .env("BUFO_CLIP", p)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .stdin(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::{fs, io::Write, process::Stdio};

    pub fn beep() {
        // Terminal bell; desktop sounds are not portable.
        let _ = std::io::stderr().write_all(b"\x07");
    }

    pub fn open(path: &Path) {
        let _ = Command::new("xdg-open").arg(path).spawn();
    }

    pub fn reveal(path: &Path) {
        // No portable "select file" on Linux: open the containing folder.
        open(path.parent().unwrap_or(path));
    }

    fn lock_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        dir.join(format!("bufo-{name}.pid"))
    }

    fn pid_alive(pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    fn read_pid(name: &str) -> Option<u32> {
        fs::read_to_string(lock_path(name)).ok()?.trim().parse().ok()
    }

    /// Record our pid; true if another live process already owns this name.
    pub fn claim_instance(name: &str) -> bool {
        if read_pid(name).is_some_and(|p| p != std::process::id() && pid_alive(p)) {
            return true;
        }
        let _ = fs::write(lock_path(name), std::process::id().to_string());
        false
    }

    pub fn instance_running(name: &str) -> bool {
        read_pid(name).is_some_and(pid_alive)
    }

    /// Local UTC offset via `date +%z` (e.g. +0300).
    pub fn local_offset_secs() -> i64 {
        let out = Command::new("date").arg("+%z").output().ok();
        let s = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        if s.len() == 5 {
            let sign = if s.starts_with('-') { -1 } else { 1 };
            let h: i64 = s[1..3].parse().unwrap_or(0);
            let m: i64 = s[3..5].parse().unwrap_or(0);
            return sign * (h * 3600 + m * 60);
        }
        0
    }

    fn autostart_file() -> Option<std::path::PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
        Some(base.join("autostart").join("bufo.desktop"))
    }

    pub fn autostart_enabled() -> bool {
        autostart_file().is_some_and(|p| p.exists())
    }

    /// Add or remove an XDG autostart entry.
    pub fn set_autostart(on: bool) -> bool {
        let Some(path) = autostart_file() else { return false };
        if !on {
            return fs::remove_file(&path).is_ok() || !path.exists();
        }
        let Ok(exe) = std::env::current_exe() else { return false };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=Bufo\nComment=Instant-replay clip recorder\nExec=\"{}\"\nTerminal=false\n",
            exe.display()
        );
        fs::write(path, body).is_ok()
    }

    /// Wayland: wl-copy, X11: xclip. Both need the `text/uri-list` type to paste as a file.
    pub fn copy_file_to_clipboard(path: &Path) -> bool {
        let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let uri = format!("file://{}\n", abs.display());
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let mut cmd = if wayland {
            let mut c = Command::new("wl-copy");
            c.args(["--type", "text/uri-list"]);
            c
        } else {
            let mut c = Command::new("xclip");
            c.args(["-selection", "clipboard", "-t", "text/uri-list"]);
            c
        };
        let Ok(mut child) = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(uri.as_bytes());
        }
        // xclip/wl-copy stay alive to serve the selection; do not wait for them.
        true
    }
}

pub use imp::*;

// Ctrl+Alt+F1..F12 switches virtual consoles on Linux, so use Ctrl+Shift there.
#[cfg(windows)]
pub const DEFAULT_HOTKEY_SAVE: &str = "Ctrl+Alt+F10";
#[cfg(windows)]
pub const DEFAULT_HOTKEY_QUIT: &str = "Ctrl+Alt+F11";
#[cfg(not(windows))]
pub const DEFAULT_HOTKEY_SAVE: &str = "Ctrl+Shift+F10";
#[cfg(not(windows))]
pub const DEFAULT_HOTKEY_QUIT: &str = "Ctrl+Shift+F11";

/// Keep a spawned helper from flashing a console window (Windows only).
#[allow(unused_variables)]
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
}
