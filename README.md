# Bufo

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
Config: `%APPDATA%\Bufo\config.toml`; flags override it (`bufo --help` style flags: `--buffer --clip --fps --bitrate --ui --export last --style crop`).

## Status
Tested on a desktop; real-game FPS impact not yet measured. License: GPLv3 (see LICENSE).

## Platforms
| | Windows 10/11 | Linux |
|---|---|---|
| Capture | Desktop Duplication (GPU, works with fullscreen games), WGC / ffmpeg fallbacks | X11 via ffmpeg `x11grab` (**experimental, untested on real hardware**); Wayland not yet |
| Encoder | Media Foundation hardware HEVC | ffmpeg: NVENC, VAAPI, or software x264 |
| Audio | WASAPI loopback (+ mic) | PulseAudio/PipeWire monitor via ffmpeg |
| Hotkeys / tray | yes | not yet (control with `save` / `quit` typed in the terminal) |
| Window app (`--ui`), gallery, 9:16 export | yes | builds; untested |

The Linux build is checked for compilation in CI only. Reports from Linux users are welcome.
