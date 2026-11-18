//! The ClipRecorder window: clip library (search, rename, tag, play, export,
//! delete) and settings. It is a separate on-demand process (`--ui`, opened
//! from the tray), so it costs nothing while you are gaming.

use std::{
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
    sync::mpsc::{channel, Receiver, Sender},
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::{
        Threading::OpenMutexW,
        Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION},
    },
};

use crate::{export, gallery, settings, Config};

const SYNCHRONIZE: u32 = 0x0010_0000;

const ACCENT: Color32 = Color32::from_rgb(230, 58, 58);
const BG: Color32 = Color32::from_rgb(15, 17, 21);
const PANEL: Color32 = Color32::from_rgb(21, 24, 30);
const CARD: Color32 = Color32::from_rgb(28, 32, 40);
const LINE: Color32 = Color32::from_rgb(42, 47, 58);
const MUTED: Color32 = Color32::from_rgb(139, 147, 163);
const OK: Color32 = Color32::from_rgb(76, 201, 120);

/// Is the background recorder running? (It holds a named mutex.)
fn recorder_running() -> bool {
    let name: Vec<u16> = "Local\\ClipRecorderSingleInstance\0".encode_utf16().collect();
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
fn local_offset_secs() -> i64 {
    unsafe {
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let kind = GetTimeZoneInformation(&mut tz);
        let extra = if kind == 2 { tz.DaylightBias } else { tz.StandardBias };
        -((tz.Bias + extra) as i64) * 60
    }
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
}

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
    max_clips_gb: f64,
    keep_days: u64,
    hotkey_save: String,
    hotkey_quit: String,
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
            max_clips_gb: c.max_clips_gb,
            keep_days: c.keep_days,
            hotkey_save: c.hotkey_save.clone(),
            hotkey_quit: c.hotkey_quit.clone(),
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
            max_clips_gb: 0.0,
            keep_days: 0,
            hotkey_save: "Ctrl+Alt+F10".into(),
            hotkey_quit: "Ctrl+Alt+F11".into(),
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
            max_clips_gb: Some(self.max_clips_gb),
            keep_days: Some(self.keep_days),
            hotkey_save: Some(self.hotkey_save.clone()),
            hotkey_quit: Some(self.hotkey_quit.clone()),
        }
    }

    fn fingerprint(&self) -> String {
        toml::to_string(&self.to_file()).unwrap_or_default()
    }

    fn hotkeys_valid(&self) -> bool {
        settings::parse_hotkey(&self.hotkey_save).is_some()
            && settings::parse_hotkey(&self.hotkey_quit).is_some()
            && self.hotkey_save.trim().to_lowercase() != self.hotkey_quit.trim().to_lowercase()
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
        };
        app.load();
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
                let _ = Command::new("explorer").arg(&self.clips[i].path).spawn();
            }
            Action::Show(i) => {
                let _ = Command::new("explorer")
                    .raw_arg(format!("/select,\"{}\"", self.clips[i].path.display()))
                    .spawn();
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
            ui.label(RichText::new("\u{25cf}").color(ACCENT).size(18.0));
            ui.label(RichText::new("ClipRecorder").strong().size(18.0));
        });
        ui.add_space(18.0);
        for (tab, icon, name) in [(Tab::Clips, "\u{25b6}", "Clips"), (Tab::Settings, "\u{2699}", "Settings")] {
            let on = self.tab == tab;
            let text = RichText::new(format!("  {icon}   {name}")).size(15.0).color(if on { Color32::WHITE } else { MUTED });
            let btn = egui::Button::new(text)
                .fill(if on { ACCENT } else { Color32::TRANSPARENT })
                .stroke(egui::Stroke::NONE)
                .corner_radius(8)
                .min_size(egui::vec2(ui.available_width(), 38.0));
            if ui.add(btn).clicked() {
                self.tab = tab;
            }
            ui.add_space(4.0);
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
                let b = egui::Button::new(RichText::new("Start recorder").color(Color32::WHITE))
                    .fill(ACCENT)
                    .corner_radius(8)
                    .min_size(egui::vec2(ui.available_width(), 32.0));
                if ui.add(b).clicked() {
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
            if ui.button("\u{27f3} Refresh").clicked() {
                self.reload = true;
            }
        });
        ui.add_space(10.0);

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
                    egui::Frame::new()
                        .fill(CARD)
                        .stroke(egui::Stroke::new(1.0, LINE))
                        .corner_radius(12)
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
                                let play = egui::Button::new(RichText::new("\u{25b6} Play").color(Color32::WHITE))
                                    .fill(ACCENT)
                                    .corner_radius(6);
                                if ui.add(play).clicked() {
                                    actions.push(Action::Play(i));
                                }
                                if ui.button("9:16").on_hover_text("Export vertical video").clicked() {
                                    actions.push(Action::Export(i));
                                }
                                if ui.button("Folder").clicked() {
                                    actions.push(Action::Show(i));
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
                            });
                        });
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
                    ui.add_enabled(f.audio, egui::Checkbox::new(&mut f.mic, "Mix the default microphone in"));
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
                row(ui, "Delete clips older than", |ui| {
                    ui.add(egui::DragValue::new(&mut f.keep_days).range(0..=3650).suffix(" days"));
                    ui.label(RichText::new("0 = never").color(MUTED).size(12.0));
                });
            });

            card(ui, "Hotkeys", |ui| {
                for (label, text) in [("Save clip", &mut f.hotkey_save), ("Quit recorder", &mut f.hotkey_quit)] {
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
                if f.hotkey_save.trim().eq_ignore_ascii_case(f.hotkey_quit.trim()) {
                    ui.label(RichText::new("The two hotkeys must differ.").color(ACCENT));
                }
            });

            card(ui, "About", |ui| {
                ui.label("ClipRecorder \u{b7} GPLv3 \u{b7} open source");
                ui.label(
                    RichText::new("Windows Graphics Capture + hardware encoder, disk ring buffer.")
                        .color(MUTED)
                        .size(12.0),
                );
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let btn = egui::Button::new(RichText::new("Save settings").color(Color32::WHITE))
                    .fill(if dirty && valid { ACCENT } else { LINE })
                    .corner_radius(8)
                    .min_size(egui::vec2(140.0, 34.0));
                if ui.add_enabled(dirty && valid, btn).clicked() {
                    save = true;
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
                    "Settings saved".into()
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
        .corner_radius(12)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(15.0));
            ui.add_space(8.0);
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
    v.extreme_bg_color = Color32::from_rgb(12, 14, 18);
    v.selection.bg_fill = ACCENT;
    v.hyperlink_color = ACCENT;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = egui::CornerRadius::same(6);
    }
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(40, 45, 56);
    v.widgets.inactive.bg_fill = Color32::from_rgb(40, 45, 56);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(54, 60, 74);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    ctx.set_visuals(v);
}

pub fn run(cfg: Config) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipRecorder")
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "ClipRecorder",
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
