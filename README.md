# ClipRecorder

Lightweight instant-replay clip recorder for Windows, written in Rust (a Medal.tv alternative).
Records the screen into a disk ring buffer with the hardware encoder, so RAM stays low; one hotkey saves the last N seconds instantly.

## Features
- Windows Graphics Capture + Media Foundation hardware HEVC, AAC audio (system + optional mic)
- Disk ring buffer (default 15 min); saving is a stream-copy concat (instant)
- Hotkeys (default `Ctrl+Alt+F10` save, `Ctrl+Alt+F11` quit), tray icon
- Window app (`--ui`): clip grid, search, game filter, rename/tags, delete, settings
- Single-file HTML gallery (`--gallery`), 9:16 export (blur or crop, trim)
- Auto-cleanup by size / age

## Requirements
Windows 10/11, ffmpeg in PATH (or `FFMPEG` env var).

## Build / test
```
cargo build --release
cargo test
```
Config: `%APPDATA%\ClipRecorder\config.toml`; flags override it (`cliprecorder --help` style flags: `--buffer --clip --fps --bitrate --ui --export last --style crop`).

## Status
Tested on a desktop; real-game FPS impact not yet measured. License: GPLv3 (see LICENSE).
