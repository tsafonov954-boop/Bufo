# Bufo

Lightweight instant-replay clip recorder, written in Rust (a Medal.tv alternative).
Records the screen into a disk ring buffer with the hardware encoder, so RAM stays low; one hotkey saves the last N seconds instantly.

## Quick start (Windows 10/11)
Download `bufo.exe` from the [latest release](../../releases/latest) and double-click it. That is the whole install:
the file already contains ffmpeg. On the first start it unpacks it to `%APPDATA%\Bufo` (a few seconds, about 220 MB on disk),
opens the window and starts recording in the tray. Press **Ctrl+Alt+F10** to save the last minute.
Windows SmartScreen may warn about an unsigned app: choose "More info" and "Run anyway".

No ads, no accounts, no tracking. The release build makes no network connections at all.

## Features
- Neon dark window UI (clip grid with previews, settings, trim editor)
- Desktop Duplication capture (works with borderless-fullscreen games) + Media Foundation hardware HEVC, AAC audio (system, plus the microphone as its own separate track)
- Disk ring buffer (default 15 min); saving is a stream-copy concat (instant)
- Hotkeys for save / long clip (default last 5 min) / screenshot / quit, tray icon, desktop notification and sound on save
- Saved clip is copied to the clipboard as a file (paste straight into chats)
- Window app (`--ui`): clip grid, search, game filter, rename/tags, favorites, delete, settings
- Built-in trim editor with a film strip, GIF export, 9:16 export (blur or crop)
- Single-file HTML gallery (`--gallery`)
- Auto-cleanup by size / age (favorites are kept)
- Start with the system (settings checkbox or `--autostart on|off|status`); saving settings restarts the recorder

## Requirements
Windows 10/11 (NVIDIA, AMD or Intel GPU with a hardware HEVC encoder) or Linux (X11).
The release build needs nothing else. A normal source build needs ffmpeg in PATH (or the `FFMPEG` env var,
or `ffmpeg.exe` next to `bufo.exe`); if none is found Bufo downloads the official essentials build once.

## Build / test
```
cargo build --release
cargo test
```
Single-file build with ffmpeg embedded (what the release ships; needs `xz`, e.g. from Git for Windows):
```
powershell -ExecutionPolicy Bypass -File scripts\make-single.ps1
```
The result is `dist\bufo.exe` (about 68 MB). ffmpeg inside is the Gyan.dev full build (GPL), its source is at https://ffmpeg.org.
Config: `%APPDATA%\Bufo\config.toml` (Linux: `~/.config/Bufo/config.toml`); flags override it.
Useful flags: `--buffer --clip --fps --bitrate --out --ui --screenshot --restart --autostart on|off|status`,
`--export last --style crop` (9:16), `--trim last --from 2 --to 7`, `--gif last`.

## Status
Tested on one desktop (RTX 3060): about 5% of one CPU core and about 110 MB of working set while recording 1080p60;
memory and the disk buffer stay flat over a 12-minute run with a 2-minute buffer. Real-game FPS impact is not measured yet.
License: GPLv3 (see LICENSE).

## Platforms
| | Windows 10/11 | Linux |
|---|---|---|
| Capture | Desktop Duplication (GPU, works with fullscreen games), WGC / ffmpeg fallbacks | X11 via ffmpeg `x11grab` (tested in WSL/Xvfb); Wayland not yet |
| Encoder | ffmpeg hardware HEVC, probed at start: NVIDIA NVENC, AMD AMF, Intel Quick Sync (AMD/Intel untested); Media Foundation HEVC as fallback | ffmpeg: NVENC, VAAPI, or software x264 (each is probed before use) |
| Audio | WASAPI loopback (+ mic) | PulseAudio/PipeWire monitor via ffmpeg (untested) |
| Hotkeys / tray | yes | X11 global hotkeys (default `Ctrl+Shift+F9/F10/F11`); no tray yet |
| Window app (`--ui`), gallery, trim / GIF / 9:16 | yes | builds; untested |

CI builds and tests both platforms. Reports from Linux users are welcome.
