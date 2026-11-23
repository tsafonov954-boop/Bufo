//! The Bufo window: clip library (search, rename, tag, play, export,
//! delete) and settings. It is a separate on-demand process (`--ui`, opened
//! from the tray), so it costs nothing while you are gaming.

use std::{
    path::PathBuf,
    process::Command,
    sync::mpsc::{channel, Receiver, Sender},
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};

use crate::{export, gallery, settings, Config};

// Gaming-neon palette: near-black violet surfaces, magenta + cyan glow.
const ACCENT: Color32 = Color32::from_rgb(255, 46, 151); // neon magenta (primary)
const NEON2: Color32 = Color32::from_rgb(0, 229, 255); // neon cyan (secondary)
const BG: Color32 = Color32::from_rgb(9, 7, 20);
const PANEL: Color32 = Color32::from_rgb(15, 12, 32);
const CARD: Color32 = Color32::from_rgb(23, 19, 46);
const LINE: Color32 = Color32::from_rgb(52, 44, 98);
const MUTED: Color32 = Color32::from_rgb(150, 144, 196);
const OK: Color32 = Color32::from_rgb(57, 255, 136);

fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

/// Soft neon halo around `rect`: a few expanding translucent outlines.
fn glow(p: &egui::Painter, rect: egui::Rect, color: Color32, radius: f32, strength: f32) {
    for i in 1..=6 {
        let t = i as f32 / 6.0;
        let a = ((1.0 - t) * 70.0 * strength) as u8;
        p.rect_stroke(
            rect.expand(t * 9.0),
            radius + t * 9.0,
            egui::Stroke::new(2.0, with_alpha(color, a)),
            egui::StrokeKind::Outside,
        );
    }
}

/// Primary action button: neon fill with a halo that brightens on hover.
fn neon_button(ui: &mut egui::Ui, text: &str, color: Color32, min: egui::Vec2) -> egui::Response {
    let b = egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
        .fill(color)
        .stroke(egui::Stroke::new(1.0, with_alpha(Color32::WHITE, 90)))
        .corner_radius(9)
        .min_size(min);
    let r = ui.add(b);
    glow(ui.painter(), r.rect, color, 9.0, if r.hovered() { 1.0 } else { 0.45 });
    r
}

/// Is the background recorder running?
fn recorder_running() -> bool {
    crate::platform::instance_running("BufoSingleInstance")
}

fn local_offset_secs() -> i64 {
    crate::platform::local_offset_secs()
}

struct Clip {
    path: PathBuf,
    meta: gallery::ClipMeta,
    size_mb: f64,
    tags_text: String,
    confirm_delete: bool,
}

enum Action {
    Play(usize),
    Show(usize),
    Export(usize),
    Delete(usize),
    SaveMeta(usize),
    Fav(usize),
    Trim(usize),
    Gif(usize),
    Copy(usize),
}

/// The open trim panel: which clip and the selected range in seconds.
struct Trim {
    path: PathBuf,
    title: String,
    dur: f64,
    from: f64,
    to: f64,
    strip_dir: PathBuf,
    prefix: String,
}

const STRIP_FRAMES: usize = 8;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Clips,
    Settings,
}

/// Editable copy of the settings.
#[derive(Clone)]
struct Form {
    clips_dir: String,
    buffer_secs: u64,
    clip_secs: u64,
    fps: u32,
    bitrate_mbps: f32,
    segment_secs: u64,
    audio: bool,
    mic: bool,
    mic_track: bool,
    clipboard: bool,
    max_clips_gb: f64,
    keep_days: u64,
    hotkey_save: String,
    hotkey_quit: String,
    hotkey_shot: String,
    hotkey_long: String,
    clip_long_secs: u64,
}

impl Form {
    fn from(c: &Config) -> Self {
        Self {
            clips_dir: c.clips_dir.display().to_string(),
            buffer_secs: c.buffer_secs,
            clip_secs: c.clip_secs,
            fps: c.fps,
            bitrate_mbps: c.bitrate as f32 / 1_000_000.0,
            segment_secs: crate::seg_secs(),
            audio: c.audio,
            mic: c.mic,
            mic_track: c.mic_track,
            clipboard: c.clipboard,
            max_clips_gb: c.max_clips_gb,
            keep_days: c.keep_days,
            hotkey_save: c.hotkey_save.clone(),
            hotkey_quit: c.hotkey_quit.clone(),
            hotkey_shot: c.hotkey_shot.clone(),
            hotkey_long: c.hotkey_long.clone(),
            clip_long_secs: c.clip_long_secs,
        }
    }

    /// Factory defaults (keeps the current clips folder).
    fn defaults(&self) -> Self {
        Self {
            clips_dir: self.clips_dir.clone(),
            buffer_secs: 900,
            clip_secs: 60,
            fps: 60,
            bitrate_mbps: 6.0,
            segment_secs: 8,
            audio: true,
            mic: false,
            mic_track: true,
            clipboard: true,
            max_clips_gb: 0.0,
            keep_days: 0,
            hotkey_save: crate::platform::DEFAULT_HOTKEY_SAVE.into(),
            hotkey_quit: crate::platform::DEFAULT_HOTKEY_QUIT.into(),
            hotkey_shot: crate::platform::DEFAULT_HOTKEY_SHOT.into(),
            hotkey_long: crate::platform::DEFAULT_HOTKEY_LONG.into(),
            clip_long_secs: 300,
        }
    }

    fn to_file(&self) -> settings::FileConfig {
        settings::FileConfig {
            clips_dir: Some(self.clips_dir.clone()),
            buffer_secs: Some(self.buffer_secs),
            clip_secs: Some(self.clip_secs),
            fps: Some(self.fps),
            bitrate: Some((self.bitrate_mbps * 1_000_000.0) as u32),
            segment_secs: Some(self.segment_secs),
            audio: Some(self.audio),
            mic: Some(self.mic),
            mic_track: Some(self.mic_track),
            clipboard: Some(self.clipboard),
            max_clips_gb: Some(self.max_clips_gb),
            keep_days: Some(self.keep_days),
            hotkey_save: Some(self.hotkey_save.clone()),
            hotkey_quit: Some(self.hotkey_quit.clone()),
            hotkey_shot: Some(self.hotkey_shot.clone()),
            hotkey_long: Some(self.hotkey_long.clone()),
            clip_long_secs: Some(self.clip_long_secs),
        }
    }

    fn fingerprint(&self) -> String {
        toml::to_string(&self.to_file()).unwrap_or_default()
    }

    fn hotkeys_valid(&self) -> bool {
        settings::parse_hotkey(&self.hotkey_save).is_some()
            && settings::parse_hotkey(&self.hotkey_quit).is_some()
            && settings::parse_hotkey(&self.hotkey_shot).is_some()
            && settings::parse_hotkey(&self.hotkey_long).is_some()
            && self.hotkeys_distinct()
    }

    fn hotkeys_distinct(&self) -> bool {
        let l = |s: &str| s.trim().to_lowercase();
        let k = [&self.hotkey_save, &self.hotkey_quit, &self.hotkey_shot, &self.hotkey_long].map(|s| l(s));
        (0..4).all(|i| (i + 1..4).all(|j| k[i] != k[j]))
    }
}

pub struct App {
    cfg: Config,
    clips: Vec<Clip>,
    query: String,
    game: String,
    tab: Tab,
    status: String,
    form: Form,
    saved_print: String,
    crop_style: bool,
    tx: Sender<String>,
    rx: Receiver<String>,
    running: bool,
    last_check: Instant,
    reload: bool,
    tz: i64,
    autostart: bool,
    trim: Option<Trim>,
    only_fav: bool,
}

impl App {
    fn new(cfg: Config) -> Self {
        let (tx, rx) = channel();
        let form = Form::from(&cfg);
        let saved_print = form.fingerprint();
        let mut app = Self {
            cfg,
            clips: Vec::new(),
            query: String::new(),
            game: String::new(),
            tab: if std::env::var("CLIPREC_TAB").as_deref() == Ok("settings") { Tab::Settings } else { Tab::Clips },
            status: String::new(),
            form,
            saved_print,
            crop_style: false,
            tx,
            rx,
            running: recorder_running(),
            last_check: Instant::now(),
            reload: false,
            tz: local_offset_secs(),
            autostart: crate::platform::autostart_enabled(),
            trim: None,
            only_fav: false,
        };
        app.load();
        // Test hook: open the trim panel for the newest clip right away.
        if std::env::var_os("CLIPREC_TRIM").is_some() && !app.clips.is_empty() {
            app.do_action(Action::Trim(0));
        }
        app
    }

    fn load(&mut self) {
        self.clips = gallery::scan(&self.cfg)
            .into_iter()
            .map(|(path, meta, size)| {
                gallery::make_thumb(&self.cfg, &path);
                Clip {
                    tags_text: meta.tags.join(", "),
                    path,
                    meta,
                    size_mb: (size as f64 / 1_048_576.0 * 10.0).round() / 10.0,
                    confirm_delete: false,
                }
            })
            .collect();
    }

    fn matches(&self, c: &Clip) -> bool {
        if !self.game.is_empty() && c.meta.game != self.game {
            return false;
        }
        if self.only_fav && !c.meta.favorite {
            return false;
        }
        let hay = format!(
            "{} {} {} {} {}",
            c.meta.title,
            c.meta.game,
            c.meta.tags.join(" "),
            date_str(c.meta.created, self.tz),
            c.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
        )
        .to_lowercase();
        self.query.to_lowercase().split_whitespace().all(|w| hay.contains(w))
    }

    fn do_action(&mut self, a: Action) {
        match a {
            Action::Play(i) => {
                crate::platform::open(&self.clips[i].path);
            }
            Action::Show(i) => {
                crate::platform::reveal(&self.clips[i].path);
            }
            Action::SaveMeta(i) => {
                let c = &mut self.clips[i];
                c.meta.tags =
                    c.tags_text.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                gallery::save_meta(&c.path, &c.meta);
                self.status = "Saved".to_string();
            }
            Action::Export(i) => {
                let path = self.clips[i].path.clone();
                let style = if self.crop_style { export::Style::Crop } else { export::Style::Blur };
                let tx = self.tx.clone();
                self.status = "Exporting 9:16 ...".to_string();
                std::thread::spawn(move || {
                    let msg = match export::vertical(&path, None, None, style) {
                        Some(p) => format!("Exported: {}", p.display()),
                        None => "Export failed (is ffmpeg installed?)".to_string(),
                    };
                    let _ = tx.send(msg);
                });
            }
            Action::Fav(i) => {
                let c = &mut self.clips[i];
                c.meta.favorite = !c.meta.favorite;
                gallery::save_meta(&c.path, &c.meta);
            }
            Action::Trim(i) => {
                let c = &self.clips[i];
                let dur = export::duration(&c.path).unwrap_or(c.meta.duration as f64).max(1.0);
                let stem = c.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let prefix = format!("strip_{stem}");
                let dir = self.cfg.clips_dir.join(".thumbs");
                let path = c.path.clone();
                {
                    let (path, dir, prefix) = (path.clone(), dir.clone(), prefix.clone());
                    std::thread::spawn(move || export::filmstrip(&path, dur, STRIP_FRAMES, &dir, &prefix));
                }
                self.trim = Some(Trim { path, title: c.meta.title.clone(), dur, from: 0.0, to: dur, strip_dir: dir, prefix });
            }
            Action::Gif(i) => {
                let path = self.clips[i].path.clone();
                let tx = self.tx.clone();
                self.status = "Making GIF (first 15 s) ...".to_string();
                std::thread::spawn(move || {
                    let msg = match export::gif(&path, 0.0, 15.0) {
                        Some(p) => format!("GIF saved: {}", p.display()),
                        None => "GIF failed (is ffmpeg installed?)".to_string(),
                    };
                    let _ = tx.send(msg);
                });
            }
            Action::Copy(i) => {
                let ok = crate::platform::copy_file_to_clipboard(&self.clips[i].path);
                self.status = if ok { "Copied to clipboard".into() } else { "Could not copy".into() };
            }
            Action::Delete(i) => {
                if self.clips[i].confirm_delete {
                    gallery::delete_clip(&self.cfg, &self.clips[i].path);
                    self.status = "Clip deleted".to_string();
                    self.reload = true;
                } else {
                    self.clips[i].confirm_delete = true;
                }
            }
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let dot = ui.label(RichText::new("\u{25cf}").color(ACCENT).size(20.0));
            glow(ui.painter(), dot.rect.shrink(3.0), ACCENT, 8.0, 0.8);
            ui.label(RichText::new("BUFO").strong().size(22.0).color(NEON2));
        });
        ui.add_space(22.0);
        for (tab, icon, name) in [(Tab::Clips, "\u{25b6}", "CLIPS"), (Tab::Settings, "\u{2699}", "SETTINGS")] {
            let on = self.tab == tab;
            let text = RichText::new(format!("  {icon}   {name}")).size(14.0).strong().color(if on { Color32::WHITE } else { MUTED });
            let btn = egui::Button::new(text)
                .fill(if on { with_alpha(ACCENT, 60) } else { Color32::TRANSPARENT })
                .stroke(if on { egui::Stroke::new(1.5, ACCENT) } else { egui::Stroke::NONE })
                .corner_radius(10)
                .min_size(egui::vec2(ui.available_width(), 40.0));
            let r = ui.add(btn);
            if on {
                glow(ui.painter(), r.rect, ACCENT, 10.0, 0.7);
            } else if r.hovered() {
                glow(ui.painter(), r.rect, NEON2, 10.0, 0.4);
            }
            if r.clicked() {
                self.tab = tab;
            }
            ui.add_space(6.0);
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(10.0);
            let total: f64 = self.clips.iter().map(|c| c.size_mb).sum();
            ui.label(
                RichText::new(format!("{} clips \u{b7} {:.1} GB", self.clips.len(), total / 1024.0))
                    .color(MUTED)
                    .size(12.0),
            );
            ui.add_space(6.0);
            if !self.running {
                if neon_button(ui, "Start recorder", ACCENT, egui::vec2(ui.available_width(), 34.0)).clicked() {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = Command::new(exe).spawn();
                    }
                }
                ui.add_space(6.0);
            }
            let (dot, text, col) =
                if self.running { ("\u{25cf}", "Recording", OK) } else { ("\u{25cb}", "Recorder stopped", MUTED) };
            ui.horizontal(|ui| {
                ui.label(RichText::new(dot).color(col));
                ui.label(RichText::new(text).color(col));
            });
        });
    }

    /// Range sliders for cutting a clip; saves a new `_trim.mp4`, the original stays.
    fn trim_panel(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.trim.as_mut() else { return };
        let (mut run, mut close, mut preview) = (false, false, false);
        egui::Frame::new().fill(CARD).stroke(egui::Stroke::new(1.5, NEON2)).corner_radius(14).inner_margin(14).show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("\u{2702}  Trim: {}", t.title)).strong().size(15.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("\u{2715}").clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(6.0);
                let dur = t.dur;
                // Film strip with the unselected parts dimmed.
                let strip_w = ui.available_width();
                let cell = strip_w / STRIP_FRAMES as f32;
                let strip_h = (cell * 9.0 / 16.0).floor();
                let (strip, _) = ui.allocate_exact_size(egui::vec2(strip_w, strip_h), egui::Sense::hover());
                ui.painter().rect_filled(strip, 6, Color32::BLACK);
                let mut loading = false;
                for k in 0..STRIP_FRAMES {
                    let f = t.strip_dir.join(format!("{}_{k}.jpg", t.prefix));
                    let r = egui::Rect::from_min_size(
                        strip.min + egui::vec2(cell * k as f32, 0.0),
                        egui::vec2(cell, strip_h),
                    );
                    if f.exists() {
                        let uri = format!("file:///{}", f.display().to_string().replace('\\', "/"));
                        egui::Image::new(uri).fit_to_exact_size(r.size()).paint_at(ui, r);
                    } else {
                        loading = true;
                    }
                }
                let x = |s: f64| strip.min.x + strip_w * (s / dur) as f32;
                let dim = Color32::from_black_alpha(165);
                ui.painter().rect_filled(egui::Rect::from_min_max(strip.min, egui::pos2(x(t.from), strip.max.y)), 0, dim);
                ui.painter().rect_filled(egui::Rect::from_min_max(egui::pos2(x(t.to), strip.min.y), strip.max), 0, dim);
                let edge = egui::Stroke::new(2.0, ACCENT);
                ui.painter().vline(x(t.from), strip.y_range(), edge);
                ui.painter().vline(x(t.to), strip.y_range(), edge);
                if loading {
                    ui.ctx().request_repaint_after(Duration::from_millis(400));
                }
                ui.add_space(6.0);
                let (mut from, mut to) = (t.from, t.to);
                egui::Grid::new("trim_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(RichText::new("Start").color(MUTED));
                    ui.add(egui::Slider::new(&mut from, 0.0..=dur).suffix(" s").fixed_decimals(1));
                    ui.end_row();
                    ui.label(RichText::new("End").color(MUTED));
                    ui.add(egui::Slider::new(&mut to, 0.0..=dur).suffix(" s").fixed_decimals(1));
                    ui.end_row();
                });
                // Keep at least half a second between the handles.
                if from > t.from && from > to - 0.5 {
                    from = (to - 0.5).max(0.0);
                }
                if to < t.to && to < from + 0.5 {
                    to = (from + 0.5).min(dur);
                }
                t.from = from;
                t.to = to;
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if neon_button(ui, "Save trimmed copy", ACCENT, egui::vec2(0.0, 30.0)).clicked() {
                        run = true;
                    }
                    if ui.button("Open original").clicked() {
                        preview = true;
                    }
                    ui.label(
                        RichText::new(format!("{:.1} s selected of {:.1} s", t.to - t.from, t.dur))
                            .color(MUTED)
                            .size(12.0),
                    );
                });
            },
        );
        if preview {
            crate::platform::open(&t.path);
        }
        if run {
            let (path, from, to) = (t.path.clone(), t.from, t.to);
            let tx = self.tx.clone();
            self.status = "Trimming ...".to_string();
            self.trim = None;
            std::thread::spawn(move || {
                let msg = match export::trim(&path, from, to) {
                    Some(p) => format!("Trimmed copy: {}", p.display()),
                    None => "Trim failed (is ffmpeg installed?)".to_string(),
                };
                let _ = tx.send(msg);
            });
        } else if close {
            self.trim = None;
        }
    }

    fn clips_ui(&mut self, ui: &mut egui::Ui) {
        // Toolbar.
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("\u{1f50d}  Search title, game, tag, date (2026-11)")
                    .desired_width(320.0)
                    .margin(egui::vec2(10.0, 7.0)),
            );
            let mut games: Vec<String> =
                self.clips.iter().map(|c| c.meta.game.clone()).filter(|g| !g.is_empty()).collect();
            games.sort();
            games.dedup();
            egui::ComboBox::from_id_salt("game")
                .selected_text(if self.game.is_empty() { "All games" } else { &self.game })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.game, String::new(), "All games");
                    for g in &games {
                        ui.selectable_value(&mut self.game, g.clone(), g);
                    }
                });
            ui.separator();
            ui.label(RichText::new("9:16 export").color(MUTED));
            ui.selectable_value(&mut self.crop_style, false, "Blur");
            ui.selectable_value(&mut self.crop_style, true, "Crop");
            ui.toggle_value(&mut self.only_fav, "Favorites only");
            if ui.button("\u{27f3} Refresh").clicked() {
                self.reload = true;
            }
        });
        ui.add_space(10.0);

        if self.trim.is_some() {
            self.trim_panel(ui);
            ui.add_space(10.0);
        }

        if self.clips.is_empty() {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No clips yet").size(24.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Press {} while the recorder is running.", self.cfg.hotkey_save))
                        .color(MUTED),
                );
            });
            return;
        }

        let visible: Vec<usize> = (0..self.clips.len()).filter(|&i| self.matches(&self.clips[i])).collect();
        if visible.is_empty() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Nothing matches your search").color(MUTED)));
            return;
        }

        let mut actions = Vec::new();
        let tz = self.tz;
        let avail = ui.available_width() - 14.0;
        let gap = 14.0;
        let cols = (((avail + gap) / (270.0 + gap)).floor() as usize).max(1);
        let card_w = ((avail - gap * (cols - 1) as f32) / cols as f32).floor();
        let inner_w = card_w - 20.0;
        let thumb_h = (inner_w * 9.0 / 16.0).floor();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
            for row_items in visible.chunks(cols) {
              ui.horizontal_top(|ui| {
                for &i in row_items {
                    let thumb = gallery::thumb_path(&self.cfg, &self.clips[i].path);
                    let clip = &mut self.clips[i];
                    let fr = egui::Frame::new()
                        .fill(CARD)
                        .stroke(egui::Stroke::new(1.0, LINE))
                        .corner_radius(14)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width(inner_w);
                            ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                            let size = egui::vec2(inner_w, thumb_h);
                            let rect = if thumb.exists() {
                                let uri = format!("file:///{}", thumb.display().to_string().replace('\\', "/"));
                                ui.add(egui::Image::new(uri).fit_to_exact_size(size).corner_radius(8)).rect
                            } else {
                                let (r, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                                ui.painter().rect_filled(r, 8, Color32::BLACK);
                                r
                            };
                            if clip.meta.duration > 0 {
                                let text = format!("{}:{:02}", clip.meta.duration / 60, clip.meta.duration % 60);
                                let galley = ui.painter().layout_no_wrap(
                                    text,
                                    egui::FontId::proportional(12.0),
                                    Color32::WHITE,
                                );
                                let pad = egui::vec2(6.0, 2.0);
                                let br = egui::Rect::from_min_size(
                                    rect.right_bottom() - galley.size() - pad * 2.0 - egui::vec2(6.0, 6.0),
                                    galley.size() + pad * 2.0,
                                );
                                ui.painter().rect_filled(br, 4, Color32::from_black_alpha(190));
                                ui.painter().galley(br.min + pad, galley, Color32::WHITE);
                            }
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut clip.meta.title)
                                    .font(egui::TextStyle::Button)
                                    .desired_width(inner_w),
                            );
                            if r.lost_focus() {
                                actions.push(Action::SaveMeta(i));
                            }
                            ui.label(
                                RichText::new(format!(
                                    "{} \u{b7} {} MB{}",
                                    date_time_str(clip.meta.created, tz),
                                    clip.size_mb,
                                    if clip.meta.game.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" \u{b7} {}", clip.meta.game)
                                    }
                                ))
                                .color(MUTED)
                                .size(12.0),
                            );
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut clip.tags_text)
                                    .hint_text("add tags, comma separated")
                                    .desired_width(inner_w),
                            );
                            if r.lost_focus() {
                                actions.push(Action::SaveMeta(i));
                            }
                            ui.horizontal(|ui| {
                                if neon_button(ui, "\u{25b6} Play", ACCENT, egui::vec2(0.0, 26.0)).clicked() {
                                    actions.push(Action::Play(i));
                                }
                                let (star, col) = if clip.meta.favorite {
                                    ("Fav \u{2713}", Color32::from_rgb(255, 196, 61))
                                } else {
                                    ("Fav", MUTED)
                                };
                                if ui
                                    .button(RichText::new(star).color(col))
                                    .on_hover_text("Favorite (kept by auto-cleanup)")
                                    .clicked()
                                {
                                    actions.push(Action::Fav(i));
                                }
                                if ui.button("\u{2702} Trim").on_hover_text("Cut the clip").clicked() {
                                    actions.push(Action::Trim(i));
                                }
                                let (label, col) = if clip.confirm_delete {
                                    ("Really?", ACCENT)
                                } else {
                                    ("\u{1f5d1}", MUTED)
                                };
                                if ui.button(RichText::new(label).color(col)).clicked() {
                                    actions.push(Action::Delete(i));
                                }
                            });
                            ui.horizontal(|ui| {
                                if ui.button("9:16").on_hover_text("Export vertical video").clicked() {
                                    actions.push(Action::Export(i));
                                }
                                if ui.button("GIF").on_hover_text("First 15 s as a GIF").clicked() {
                                    actions.push(Action::Gif(i));
                                }
                                if ui.button("Copy").on_hover_text("Copy the file (paste into chats)").clicked() {
                                    actions.push(Action::Copy(i));
                                }
                                if ui.button("Folder").clicked() {
                                    actions.push(Action::Show(i));
                                }
                            });
                            });
                        });
                    // Neon outline on the card under the pointer.
                    if ui.rect_contains_pointer(fr.response.rect) {
                        glow(ui.painter(), fr.response.rect, NEON2, 14.0, 0.7);
                        ui.painter().rect_stroke(
                            fr.response.rect,
                            14.0,
                            egui::Stroke::new(1.5, NEON2),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
              });
            }
        });
        for a in actions {
            self.do_action(a);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let dirty = self.form.fingerprint() != self.saved_print;
        let valid = self.form.hotkeys_valid();
        let mut save = false;
        let mut reset = false;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(760.0);
            ui.label(RichText::new("Settings").size(22.0).strong());
            ui.add_space(8.0);
            let f = &mut self.form;

            card(ui, "Recording", |ui| {
                row(ui, "Frame rate", |ui| {
                    for v in [30u32, 60, 120] {
                        ui.selectable_value(&mut f.fps, v, format!("{v}"));
                    }
                    ui.add(egui::DragValue::new(&mut f.fps).range(15..=240).suffix(" fps"));
                });
                row(ui, "Quality", |ui| {
                    for (name, v) in [("Low", 4.0f32), ("Balanced", 6.0), ("High", 10.0), ("Ultra", 16.0)] {
                        let on = (f.bitrate_mbps - v).abs() < 0.05;
                        if ui.selectable_label(on, name).clicked() {
                            f.bitrate_mbps = v;
                        }
                    }
                });
                row(ui, "Bitrate", |ui| {
                    ui.add(egui::Slider::new(&mut f.bitrate_mbps, 1.0..=60.0).suffix(" Mbit/s"));
                });
                row(ui, "Buffer length", |ui| {
                    for (name, v) in [("1 min", 60u64), ("5 min", 300), ("15 min", 900), ("30 min", 1800)] {
                        if ui.selectable_label(f.buffer_secs == v, name).clicked() {
                            f.buffer_secs = v;
                        }
                    }
                    ui.add(egui::DragValue::new(&mut f.buffer_secs).range(30..=7200).suffix(" s"));
                });
                row(ui, "Saved clip length", |ui| {
                    for (name, v) in [("15 s", 15u64), ("30 s", 30), ("1 min", 60), ("2 min", 120)] {
                        if ui.selectable_label(f.clip_secs == v, name).clicked() {
                            f.clip_secs = v;
                        }
                    }
                    ui.add(egui::DragValue::new(&mut f.clip_secs).range(5..=600).suffix(" s"));
                });
                row(ui, "Long clip length", |ui| {
                    ui.add(egui::DragValue::new(&mut f.clip_long_secs).range(30..=7200).suffix(" s"));
                    ui.label(RichText::new("saved by the long-clip hotkey").color(MUTED).size(12.0));
                });
                row(ui, "Segment length", |ui| {
                    ui.add(egui::DragValue::new(&mut f.segment_secs).range(2..=30).suffix(" s"));
                    ui.label(RichText::new("longer = fewer encoder restarts").color(MUTED).size(12.0));
                });
                let gb = f.bitrate_mbps as f64 / 8.0 * f.buffer_secs as f64 / 1024.0;
                ui.label(
                    RichText::new(format!(
                        "The buffer will use about {gb:.2} GB of disk (plus audio). A saved clip of {} s is about {:.0} MB.",
                        f.clip_secs,
                        f.bitrate_mbps as f64 / 8.0 * f.clip_secs as f64
                    ))
                    .color(MUTED)
                    .size(12.0),
                );
            });

            card(ui, "Audio", |ui| {
                row(ui, "System audio", |ui| {
                    ui.checkbox(&mut f.audio, "Record game / desktop sound");
                });
                row(ui, "Microphone", |ui| {
                    ui.add_enabled(f.audio, egui::Checkbox::new(&mut f.mic, "Record the default microphone"));
                    ui.add_enabled(f.audio && f.mic, egui::Checkbox::new(&mut f.mic_track, "...as a separate audio track"));
                });
            });

            card(ui, "Storage", |ui| {
                row(ui, "Clips folder", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.clips_dir).desired_width(380.0));
                });
                row(ui, "Delete oldest above", |ui| {
                    ui.add(egui::DragValue::new(&mut f.max_clips_gb).range(0.0..=5000.0).speed(0.5).suffix(" GB"));
                    ui.label(RichText::new("0 = never").color(MUTED).size(12.0));
                });
                row(ui, "Clipboard", |ui| {
                    ui.checkbox(&mut f.clipboard, "Copy each saved clip to the clipboard (Ctrl+V in chats)");
                });
                row(ui, "Delete clips older than", |ui| {
                    ui.add(egui::DragValue::new(&mut f.keep_days).range(0..=3650).suffix(" days"));
                    ui.label(RichText::new("0 = never").color(MUTED).size(12.0));
                });
            });

            card(ui, "Startup", |ui| {
                row(ui, "Autostart", |ui| {
                    if ui.checkbox(&mut self.autostart, "Start Bufo when I log in").changed()
                        && !crate::platform::set_autostart(self.autostart)
                    {
                        self.autostart = crate::platform::autostart_enabled();
                    }
                });
            });

            card(ui, "Hotkeys", |ui| {
                for (label, text) in [
                    ("Save clip", &mut f.hotkey_save),
                    ("Save long clip", &mut f.hotkey_long),
                    ("Screenshot", &mut f.hotkey_shot),
                    ("Quit recorder", &mut f.hotkey_quit),
                ] {
                    row(ui, label, |ui| {
                        let ok = settings::parse_hotkey(text).is_some();
                        let mut e = egui::TextEdit::singleline(text).desired_width(200.0);
                        if !ok {
                            e = e.text_color(ACCENT);
                        }
                        ui.add(e);
                        if !ok {
                            ui.label(RichText::new("invalid").color(ACCENT));
                        }
                    });
                }
                ui.label(
                    RichText::new("Examples: Ctrl+Alt+F10, Shift+F9, Win+R. Alt+F10 is taken by NVIDIA's overlay.")
                        .color(MUTED)
                        .size(12.0),
                );
                if !f.hotkeys_distinct() {
                    ui.label(RichText::new("The hotkeys must all differ.").color(ACCENT));
                }
            });

            card(ui, "About", |ui| {
                ui.label("Bufo \u{b7} GPLv3 \u{b7} open source \u{b7} no ads, no tracking");
                ui.label(
                    RichText::new("Desktop Duplication + hardware HEVC encoder, rolling disk buffer.")
                        .color(MUTED)
                        .size(12.0),
                );
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if dirty && valid {
                    if neon_button(ui, "Save settings", ACCENT, egui::vec2(150.0, 36.0)).clicked() {
                        save = true;
                    }
                } else {
                    let btn = egui::Button::new(RichText::new("Save settings").color(MUTED))
                        .fill(LINE)
                        .corner_radius(9)
                        .min_size(egui::vec2(150.0, 36.0));
                    ui.add_enabled(false, btn);
                }
                if ui.add(egui::Button::new("Reset to defaults").min_size(egui::vec2(0.0, 34.0))).clicked() {
                    reset = true;
                }
                if dirty {
                    ui.label(RichText::new("\u{25cf} unsaved changes").color(MUTED));
                } else {
                    ui.label(RichText::new("\u{2713} all saved").color(OK));
                }
            });
            ui.label(
                RichText::new("Changes apply the next time the recorder starts (tray: Quit, then Start recorder).")
                    .color(MUTED)
                    .size(12.0),
            );
            ui.add_space(20.0);
        });

        if reset {
            self.form = self.form.defaults();
        }
        if save {
            self.status = match settings::save(&self.form.to_file()) {
                Ok(()) => {
                    self.saved_print = self.form.fingerprint();
                    if self.running {
                        // The recorder reads settings at start: restart it in the background.
                        let tx = self.tx.clone();
                        std::thread::spawn(move || {
                            let ok = crate::platform::restart_recorder();
                            let _ = tx.send(
                                if ok { "Settings saved, recorder restarted" } else { "Settings saved (restart the recorder)" }
                                    .to_string(),
                            );
                        });
                        "Settings saved, restarting recorder ...".into()
                    } else {
                        "Settings saved".into()
                    }
                }
                Err(e) => format!("Could not save: {e}"),
            };
        }
    }
}

fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0, LINE))
        .corner_radius(14)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // Neon tick before the card title.
                let (tick, _) = ui.allocate_exact_size(egui::vec2(4.0, 16.0), egui::Sense::hover());
                ui.painter().rect_filled(tick, 2, NEON2);
                glow(ui.painter(), tick, NEON2, 2.0, 0.6);
                ui.label(RichText::new(title.to_uppercase()).strong().size(13.0).color(Color32::WHITE));
            });
            ui.add_space(10.0);
            body(ui);
        });
    ui.add_space(12.0);
}

fn row(ui: &mut egui::Ui, label: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([170.0, 24.0], egui::Label::new(RichText::new(label).color(MUTED)));
        body(ui);
    });
    ui.add_space(2.0);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            self.status = msg;
            self.reload = true;
        }
        if self.reload {
            self.reload = false;
            self.load();
        }
        if self.last_check.elapsed() > Duration::from_secs(2) {
            self.running = recorder_running();
            self.last_check = Instant::now();
        }
        ui.ctx().request_repaint_after(Duration::from_secs(2));

        egui::Panel::left("side")
            .exact_size(190.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(12))
            .show(ui, |ui| self.sidebar(ui));
        egui::Panel::bottom("bottom")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(14, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let shown = self.clips.iter().filter(|c| self.matches(c)).count();
                    ui.label(RichText::new(format!("{shown} of {} clips", self.clips.len())).color(MUTED));
                    ui.separator();
                    ui.label(RichText::new(&self.status).color(MUTED));
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(18))
            .show(ui, |ui| match self.tab {
                Tab::Clips => self.clips_ui(ui),
                Tab::Settings => self.settings_ui(ui),
            });
    }
}

fn date_str(secs: u64, tz: i64) -> String {
    let (y, m, d, ..) = civil(secs, tz);
    format!("{y:04}-{m:02}-{d:02}")
}

fn date_time_str(secs: u64, tz: i64) -> String {
    let (y, m, d, h, mi) = civil(secs, tz);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// Unix seconds -> (year, month, day, hour, minute) shifted by `tz` seconds
/// (Howard Hinnant's algorithm).
fn civil(secs: u64, tz: i64) -> (i64, u32, u32, u32, u32) {
    let secs = (secs as i64 + tz).max(0) as u64;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32)
}

fn theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = Color32::from_rgb(7, 5, 16);
    v.selection.bg_fill = with_alpha(ACCENT, 170);
    v.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = NEON2;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = egui::CornerRadius::same(8);
    }
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 29, 66);
    v.widgets.inactive.bg_fill = Color32::from_rgb(34, 29, 66);
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, LINE);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(48, 40, 92);
    v.widgets.hovered.bg_fill = Color32::from_rgb(48, 40, 92);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.5, NEON2);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.5, ACCENT);
    v.widgets.active.weak_bg_fill = with_alpha(ACCENT, 90);
    ctx.set_visuals(v);
}

pub fn run(cfg: Config) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Bufo")
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Bufo",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            theme(&cc.egui_ctx);
            Ok(Box::new(App::new(cfg)))
        }),
    );
    if let Err(e) = result {
        settings::log(&format!("ui failed: {e}"));
    }
}
