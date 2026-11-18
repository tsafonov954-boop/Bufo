//! Which program is the user in? Used to tag clips with the game they were
//! recorded in. A tracker thread remembers the last "real" foreground app, so
//! saving from the tray menu (where Explorer is in front) still gets the game.

use std::{path::Path, sync::Mutex, thread, time::Duration};

use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION},
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

static LAST_APP: Mutex<String> = Mutex::new(String::new());

/// Shell and system hosts that are never "the game".
const IGNORED: &[&str] = &[
    "explorer",
    "cliprecorder",
    "shellexperiencehost",
    "searchhost",
    "searchapp",
    "startmenuexperiencehost",
    "applicationframehost",
    "textinputhost",
    "lockapp",
    "taskmgr",
    "dwm",
];

/// Executable name (without extension) of the foreground window's process.
pub fn foreground_app() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&path).file_stem().map(|s| s.to_string_lossy().to_string())
    }
}

/// Start remembering the last non-system foreground app.
pub fn start_tracker() {
    thread::spawn(|| loop {
        if let Some(app) = foreground_app() {
            if !IGNORED.contains(&app.to_lowercase().as_str()) {
                if let Ok(mut g) = LAST_APP.lock() {
                    *g = app;
                }
            }
        }
        thread::sleep(Duration::from_millis(1500));
    });
}

/// The last app seen in front (empty if none yet).
pub fn last_app() -> String {
    LAST_APP.lock().map(|g| g.clone()).unwrap_or_default()
}
