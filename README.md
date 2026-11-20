# Bufo

Lightweight instant-replay clip recorder, written in Rust (a Medal.tv alternative).
Records the screen into a disk ring buffer with the hardware encoder, so RAM stays low; one hotkey saves the last N seconds instantly.

## Features
- Desktop Duplication capture (works with borderless-fullscreen games) + Media Foundation hardware HEVC, AAC audio (system + optional mic)
- Disk ring buffer (default 15 min); saving is a stream-copy concat (instant)
- Hotkeys for save / screenshot / quit, tray icon, desktop notification and sound on save
- Saved clip is copied to the clipboard as a file (paste straight into chats)
- Window app (`--ui`): clip grid, search, game filter, rename/tags, favorites, delete, settings
- Built-in trim editor with a film strip, GIF export, 9:16 export (blur or crop)
- Single-file HTML gallery (`--gallery`)
- Auto-cleanup by size / age (favorites are kept)
- Start with the system (settings checkbox or `--autostart on|off|status`); saving settings restarts the recorder

## Requirements
ffmpeg in PATH (or the `FFMPEG` env var). Windows 10/11 or Linux (X11).

## Build / test
```
cargo build --release
cargo test
```
Config: `%APPDATA%\Bufo\config.toml` (Linux: `~/.config/Bufo/config.toml`); flags override it.
Useful flags: `--buffer --clip --fps --bitrate --out --ui --screenshot --restart --autostart on|off|status`,
`--export last --style crop` (9:16), `--trim last --from 2 --to 7`, `--gif last`.

## Status
Tested on a desktop; real-game FPS impact not yet measured. License: GPLv3 (see LICENSE).

## Platforms
| | Windows 10/11 | Linux |
|---|---|---|
| Capture | Desktop Duplication (GPU, works with fullscreen games), WGC / ffmpeg fallbacks | X11 via ffmpeg `x11grab` (tested in WSL/Xvfb); Wayland not yet |
| Encoder | Media Foundation hardware HEVC | ffmpeg: NVENC, VAAPI, or software x264 (each is probed before use) |
| Audio | WASAPI loopback (+ mic) | PulseAudio/PipeWire monitor via ffmpeg (untested) |
| Hotkeys / tray | yes | X11 global hotkeys (default `Ctrl+Shift+F9/F10/F11`); no tray yet |
| Window app (`--ui`), gallery, trim / GIF / 9:16 | yes | builds; untested |

CI builds and tests both platforms. Reports from Linux users are welcome.
