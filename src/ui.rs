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

use eframe::egui;
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::OpenMutexW,
};

use crate::{export, gallery, settings, Config};

const SYNCHRONIZE: u32 = 0x0010_0000;

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

#[derive(PartialEq)]
enum Tab {
    Clips,
    Settings,
}

/// Editable copy of the settings.
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
}

pub struct App {
    cfg: Config,
    clips: Vec<Clip>,
    query: String,
    game: String,
    tab: Tab,
    status: String,
    form: Form,
    crop_style: bool,
    tx: Sender<String>,
    rx: Receiver<String>,
    running: bool,
    last_check: Instant,
    reload: bool,
}

impl App {
    fn new(cfg: Config) -> Self {
        let (tx, rx) = channel();
        let form = Form::from(&cfg);
        let mut app = Self {
            cfg,
            clips: Vec::new(),
            query: String::new(),
            game: String::new(),
            tab: Tab::Clips,
            status: String::new(),
            form,
            crop_style: false,
            tx,
            rx,
            running: recorder_running(),
            last_check: Instant::now(),
            reload: false,
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
            date_str(c.meta.created),
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

    fn clips_ui(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        let visible: Vec<usize> = (0..self.clips.len()).filter(|&i| self.matches(&self.clips[i])).collect();
        if self.clips.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.heading("No clips yet");
                ui.label(format!("Press {} while the recorder is running.", self.cfg.hotkey_save));
            });
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for i in visible {
                    let thumb = gallery::thumb_path(&self.cfg, &self.clips[i].path);
                    let clip = &mut self.clips[i];
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(270.0);
                        ui.vertical(|ui| {
                            let size = egui::vec2(270.0, 152.0);
                            if thumb.exists() {
                                let uri = format!("file:///{}",thumb.display().to_string().replace('\\', "/"));
                                ui.add(egui::Image::new(uri).fit_to_exact_size(size));
                            } else {
                                ui.allocate_space(size);
                            }
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut clip.meta.title).desired_width(f32::INFINITY),
                            );
                            if r.lost_focus() {
                                actions.push(Action::SaveMeta(i));
                            }
                            ui.weak(format!(
                                "{}  \u{b7}  {} MB{}",
                                date_time_str(clip.meta.created),
                                clip.size_mb,
                                if clip.meta.game.is_empty() {
                                    String::new()
                                } else {
                                    format!("  \u{b7}  {}", clip.meta.game)
                                }
                            ));
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut clip.tags_text)
                                    .hint_text("tags, comma separated")
                                    .desired_width(f32::INFINITY),
                            );
                            if r.lost_focus() {
                                actions.push(Action::SaveMeta(i));
                            }
                            ui.horizontal(|ui| {
                                if ui.button("Play").clicked() {
                                    actions.push(Action::Play(i));
                                }
                                if ui.button("9:16").on_hover_text("Export vertical video").clicked() {
                                    actions.push(Action::Export(i));
                                }
                                if ui.button("Folder").clicked() {
                                    actions.push(Action::Show(i));
                                }
                                let label = if clip.confirm_delete { "Really delete?" } else { "Delete" };
                                if ui.button(label).clicked() {
                                    actions.push(Action::Delete(i));
                                }
                            });
                        });
                    });
                }
            });
        });
        for a in actions {
            self.do_action(a);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let f = &mut self.form;
            egui::Grid::new("settings").num_columns(2).spacing([20.0, 8.0]).show(ui, |ui| {
                ui.label("Clips folder");
                ui.add(egui::TextEdit::singleline(&mut f.clips_dir).desired_width(420.0));
                ui.end_row();
                ui.label("Buffer length (s)");
                ui.add(egui::DragValue::new(&mut f.buffer_secs).range(30..=7200));
                ui.end_row();
                ui.label("Clip length (s)");
                ui.add(egui::DragValue::new(&mut f.clip_secs).range(5..=600));
                ui.end_row();
                ui.label("FPS");
                ui.add(egui::DragValue::new(&mut f.fps).range(15..=240));
                ui.end_row();
                ui.label("Video bitrate (Mbit/s)");
                ui.add(egui::DragValue::new(&mut f.bitrate_mbps).range(1.0..=60.0).speed(0.1));
                ui.end_row();
                ui.label("Segment length (s)");
                ui.add(egui::DragValue::new(&mut f.segment_secs).range(2..=30));
                ui.end_row();
                ui.label("System audio");
                ui.checkbox(&mut f.audio, "record");
                ui.end_row();
                ui.label("Microphone");
                ui.checkbox(&mut f.mic, "mix into the audio track");
                ui.end_row();
                ui.label("Auto-delete above (GB)");
                ui.add(egui::DragValue::new(&mut f.max_clips_gb).range(0.0..=5000.0).speed(0.5));
                ui.label("0 = never");
                ui.end_row();
                ui.label("Auto-delete older than (days)");
                ui.add(egui::DragValue::new(&mut f.keep_days).range(0..=3650));
                ui.label("0 = never");
                ui.end_row();
                ui.label("Save-clip hotkey");
                ui.add(egui::TextEdit::singleline(&mut f.hotkey_save).desired_width(180.0));
                ui.end_row();
                ui.label("Quit hotkey");
                ui.add(egui::TextEdit::singleline(&mut f.hotkey_quit).desired_width(180.0));
                ui.end_row();
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Save settings").clicked() {
                    self.status = match settings::save(&self.form.to_file()) {
                        Ok(()) => "Settings saved. Restart the recorder (tray: Quit, then start it) to apply.".into(),
                        Err(e) => format!("Could not save: {e}"),
                    };
                }
                ui.weak("Hotkey format: Ctrl+Alt+F10, Shift+F9, Win+R ...");
            });
        });
    }
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

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Clips, "Clips");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.separator();
                if self.tab == Tab::Clips {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("Search: title, game, tag, date (2026-11)")
                            .desired_width(300.0),
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
                    ui.checkbox(&mut self.crop_style, "9:16 crop (else blur)");
                    if ui.button("Refresh").clicked() {
                        self.reload = true;
                    }
                }
            });
            ui.add_space(6.0);
        });
        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(if self.running { "\u{25cf} Recorder running" } else { "\u{25cb} Recorder stopped" });
                if !self.running && ui.button("Start recorder").clicked() {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = Command::new(exe).spawn();
                    }
                }
                ui.separator();
                let shown = self.clips.iter().filter(|c| self.matches(c)).count();
                ui.label(format!("{shown} of {} clips", self.clips.len()));
                ui.separator();
                ui.label(&self.status);
            });
        });
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Clips => self.clips_ui(ui),
            Tab::Settings => self.settings_ui(ui),
        });
    }
}

fn date_str(secs: u64) -> String {
    let (y, m, d, ..) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

fn date_time_str(secs: u64) -> String {
    let (y, m, d, h, mi) = civil(secs);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// Unix seconds -> (year, month, day, hour, minute) in UTC (Howard Hinnant's algorithm).
fn civil(secs: u64) -> (i64, u32, u32, u32, u32) {
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

pub fn run(cfg: Config) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipRecorder")
            .with_inner_size([1120.0, 720.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "ClipRecorder",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(App::new(cfg)))
        }),
    );
    if let Err(e) = result {
        settings::log(&format!("ui failed: {e}"));
    }
}
