# ClipRecorder

Lightweight instant-replay clip recorder in Rust (Medal-style, but low RAM).

## Plan
1. MVP: Windows Graphics Capture -> GPU encode (NVENC/AMF/QSV) -> ring buffer of encoded packets -> hotkey saves mp4 without re-encoding.
2. Local gallery + search (SQLite FTS5).
3. P2P sharing (iroh), then public feed.
4. Monetisation: ads first (cover servers), subscription later.
