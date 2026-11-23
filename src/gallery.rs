//! Clip library: a small JSON sidecar per clip (game, title, tags), a JPEG
//! thumbnail, and a generated single-file HTML gallery with search. There is no
//! resident UI process, so the gallery costs no memory while you are gaming.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};

use crate::{ffmpeg_bin, Config};

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct ClipMeta {
    pub title: String,
    /// Program that was in front when the clip was saved.
    pub game: String,
    /// Unix seconds.
    pub created: u64,
    /// Approximate length in seconds.
    pub duration: u64,
    pub tags: Vec<String>,
    /// Starred by the user (never auto-deleted by cleanup).
    pub favorite: bool,
}

pub fn sidecar(clip: &Path) -> PathBuf {
    clip.with_extension("json")
}

/// Is this clip starred? Reads the sidecar; missing or broken means no.
pub fn is_favorite(clip: &Path) -> bool {
    fs::read_to_string(sidecar(clip))
        .ok()
        .and_then(|t| serde_json::from_str::<ClipMeta>(&t).ok())
        .is_some_and(|m| m.favorite)
}

/// Persist edited title/tags next to the clip.
pub fn save_meta(clip: &Path, meta: &ClipMeta) {
    if let Ok(json) = serde_json::to_string_pretty(meta) {
        let _ = fs::write(sidecar(clip), json);
    }
}

/// Remove a clip together with its sidecar and thumbnail.
pub fn delete_clip(cfg: &Config, clip: &Path) {
    let _ = fs::remove_file(clip);
    let _ = fs::remove_file(sidecar(clip));
    let _ = fs::remove_file(thumb_path(cfg, clip));
}

/// All `clip_*.mp4` with their metadata (defaults filled in) and size, newest first.
pub fn scan(cfg: &Config) -> Vec<(PathBuf, ClipMeta, u64)> {
    let Ok(rd) = fs::read_dir(&cfg.clips_dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.filter_map(|e| e.ok()) {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("clip_") && name.ends_with(".mp4")) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let mut meta: ClipMeta = fs::read_to_string(sidecar(&path))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if meta.created == 0 {
            meta.created = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
        }
        if meta.title.is_empty() {
            meta.title = if meta.game.is_empty() { "Clip".to_string() } else { meta.game.clone() };
        }
        out.push((path, meta, md.len()));
    }
    out.sort_by(|a, b| b.1.created.cmp(&a.1.created));
    out
}

pub fn thumb_path(cfg: &Config, clip: &Path) -> PathBuf {
    let stem = clip.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    cfg.clips_dir.join(".thumbs").join(format!("{stem}.jpg"))
}

/// Write the sidecar and thumbnail for a freshly saved clip.
pub fn register(cfg: &Config, clip: &Path, game: &str, duration: u64) {
    let created = fs::metadata(clip)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let title = if game.is_empty() { "Clip".to_string() } else { game.to_string() };
    let meta = ClipMeta { title, game: game.to_string(), created, duration, tags: Vec::new(), favorite: false };
    if let Ok(json) = serde_json::to_string_pretty(&meta) {
        let _ = fs::write(sidecar(clip), json);
    }
    make_thumb(cfg, clip);
}

/// One small JPEG frame (about 1 s in, or the first frame for short clips).
pub fn make_thumb(cfg: &Config, clip: &Path) {
    let thumb = thumb_path(cfg, clip);
    if thumb.exists() {
        return;
    }
    if let Some(dir) = thumb.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = crate::platform::command(ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "1", "-i"])
        .arg(clip)
        .args(["-frames:v", "1", "-vf", "scale=360:-2,format=yuvj420p", "-q:v", "5"])
        .arg(&thumb)
        .stdin(Stdio::null())
        .status();
}

#[derive(Serialize)]
struct Entry {
    file: String,
    thumb: String,
    title: String,
    game: String,
    created: u64,
    duration: u64,
    size_mb: f64,
    tags: Vec<String>,
}

fn collect(cfg: &Config) -> Vec<Entry> {
    let mut out = Vec::new();
    for (path, meta, size) in scan(cfg) {
        make_thumb(cfg, &path);
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        out.push(Entry {
            file: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            thumb: format!(".thumbs/{stem}.jpg"),
            title: meta.title,
            game: meta.game,
            created: meta.created,
            duration: meta.duration,
            size_mb: (size as f64 / 1_048_576.0 * 10.0).round() / 10.0,
            tags: meta.tags,
        });
    }
    out
}

/// Build `gallery.html` in the clips folder and return its path.
pub fn build(cfg: &Config) -> Option<PathBuf> {
    fs::create_dir_all(&cfg.clips_dir).ok()?;
    let entries = collect(cfg);
    // "</" inside inline JSON could close the script tag early.
    let json = serde_json::to_string(&entries).ok()?.replace("</", "<\\/");
    let html = PAGE.replace("/*DATA*/[]", &json);
    let path = cfg.clips_dir.join("gallery.html");
    fs::write(&path, html).ok()?;
    Some(path)
}

/// Open a file with its default program (the gallery opens in the browser).
pub fn open(path: &Path) {
    crate::platform::open(path);
}

const PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Bufo</title>
<style>
:root{--bg:#0f1115;--card:#181b22;--line:#262a33;--text:#e8eaf0;--mute:#8b93a3;--acc:#e63a3a}
@media (prefers-color-scheme:light){:root{--bg:#f4f5f8;--card:#fff;--line:#dfe2ea;--text:#1b1e26;--mute:#6a7283}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:15px/1.4 system-ui,Segoe UI,sans-serif}
header{position:sticky;top:0;z-index:5;background:var(--bg);border-bottom:1px solid var(--line);padding:14px 16px}
h1{margin:0 0 10px;font-size:18px;display:flex;align-items:center;gap:8px}
h1::before{content:"";width:12px;height:12px;border-radius:50%;background:var(--acc)}
#q{width:100%;padding:10px 12px;border-radius:10px;border:1px solid var(--line);background:var(--card);color:var(--text);font:inherit}
#chips{display:flex;flex-wrap:wrap;gap:6px;margin-top:10px}
.chip{border:1px solid var(--line);background:var(--card);color:var(--mute);border-radius:99px;padding:4px 12px;cursor:pointer;font:inherit;font-size:13px}
.chip.on{background:var(--acc);border-color:var(--acc);color:#fff}
#count{color:var(--mute);font-size:13px;margin-top:8px}
main{padding:16px;display:grid;grid-template-columns:repeat(auto-fill,minmax(250px,1fr));gap:14px}
.card{background:var(--card);border:1px solid var(--line);border-radius:12px;overflow:hidden;cursor:pointer}
.card:hover{border-color:var(--acc)}
.thumb{aspect-ratio:16/9;background:#000 center/cover no-repeat;position:relative}
.dur{position:absolute;right:6px;bottom:6px;background:#000b;color:#fff;font-size:12px;padding:1px 6px;border-radius:4px}
.info{padding:10px 12px}
.t{font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.m{color:var(--mute);font-size:12px;margin-top:2px}
.tag{display:inline-block;margin:6px 4px 0 0;font-size:11px;color:var(--mute);border:1px solid var(--line);border-radius:99px;padding:0 8px}
#empty{display:none;padding:60px 16px;text-align:center;color:var(--mute)}
#modal{position:fixed;inset:0;background:#000d;display:none;align-items:center;justify-content:center;z-index:9;padding:16px}
#modal.on{display:flex}
#modal video{max-width:100%;max-height:88vh;border-radius:8px;background:#000}
@media (max-width:520px){main{grid-template-columns:1fr}}
</style></head><body>
<header>
<h1>Bufo</h1>
<input id="q" type="search" placeholder="Search by title, game, tag, date (2026-11)..." autofocus>
<div id="chips"></div><div id="count"></div>
</header>
<main id="grid"></main>
<div id="empty">No clips found.</div>
<div id="modal"><video id="player" controls></video></div>
<script>
const clips = /*DATA*/[];
const q = document.getElementById('q'), grid = document.getElementById('grid');
const chips = document.getElementById('chips'), count = document.getElementById('count');
const empty = document.getElementById('empty'), modal = document.getElementById('modal');
const player = document.getElementById('player');
let game = '';
const day = c => new Date(c.created * 1000).toISOString().slice(0, 10);
const when = c => new Date(c.created * 1000).toLocaleString();
const hay = c => (c.title + ' ' + c.game + ' ' + c.tags.join(' ') + ' ' + day(c) + ' ' + c.file).toLowerCase();
const mmss = s => s ? Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0') : '';
function el(tag, cls, text) { const e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; }
function drawChips() {
  chips.textContent = '';
  const games = [...new Set(clips.map(c => c.game).filter(Boolean))].sort();
  if (!games.length) return;
  for (const g of ['', ...games]) {
    const b = el('button', 'chip' + (g === game ? ' on' : ''), g || 'All');
    b.onclick = () => { game = g; drawChips(); draw(); };
    chips.appendChild(b);
  }
}
function draw() {
  const words = q.value.toLowerCase().split(/\s+/).filter(Boolean);
  const list = clips.filter(c => (!game || c.game === game) && words.every(w => hay(c).includes(w)));
  grid.textContent = '';
  for (const c of list) {
    const card = el('div', 'card');
    const th = el('div', 'thumb');
    th.style.backgroundImage = 'url("' + encodeURI(c.thumb) + '")';
    if (c.duration) th.appendChild(el('span', 'dur', mmss(c.duration)));
    const info = el('div', 'info');
    info.appendChild(el('div', 't', c.title));
    info.appendChild(el('div', 'm', when(c) + ' · ' + c.size_mb + ' MB'));
    for (const t of c.tags) info.appendChild(el('span', 'tag', t));
    card.append(th, info);
    card.onclick = () => { player.src = encodeURI(c.file); modal.classList.add('on'); player.play(); };
    grid.appendChild(card);
  }
  count.textContent = list.length + ' of ' + clips.length + ' clips';
  empty.style.display = list.length ? 'none' : 'block';
}
const close = () => { player.pause(); player.removeAttribute('src'); modal.classList.remove('on'); };
modal.onclick = e => { if (e.target === modal) close(); };
addEventListener('keydown', e => { if (e.key === 'Escape') close(); });
q.oninput = draw;
drawChips(); draw();
</script></body></html>
"##;
