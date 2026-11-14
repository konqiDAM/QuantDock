// No console window for the released binary.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use quantdock::monitor_config::{self, Config, Corner, Watcher};
use quantdock::{model, server};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Editable copy of a model's .toml fields, so the file is only written on demand.
struct Draft {
    alias: String,
    args: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Models,
    Settings,
}

struct Manager {
    models: Vec<model::Model>,
    drafts: HashMap<PathBuf, Draft>,
    status: Option<server::Status>,
    last_poll: Instant,
    message: Option<(String, bool)>,
    message_at: Instant,
    root: PathBuf,
    /// Load runs off the UI thread (it stops the old server and waits ~2s).
    load_tx: std::sync::mpsc::Sender<Result<String, String>>,
    load_rx: std::sync::mpsc::Receiver<Result<String, String>>,
    tab: Tab,
    cfg: Config,
    cfg_err: Option<String>,
    cfg_watcher: Watcher,
    /// Debounce for writing settings.toml.
    dirty_at: Option<Instant>,
}

impl Manager {
    fn new() -> Self {
        let root = model::models_root();
        let (load_tx, load_rx) = std::sync::mpsc::channel();
        let path = monitor_config::settings_path();
        let (cfg, cfg_err) = match Config::load(&path) {
            Ok(c) => (c, None),
            Err(e) => {
                // First run has no file: create it. A broken one is never overwritten.
                let defaults = Config::defaults();
                let first_run = !path.exists();
                if first_run {
                    let _ = defaults.save(&path);
                    (defaults, None)
                } else {
                    (defaults, Some(e))
                }
            }
        };
        let mut m = Self {
            models: Vec::new(),
            drafts: HashMap::new(),
            status: None,
            last_poll: Instant::now() - Duration::from_secs(5),
            message: None,
            message_at: Instant::now(),
            root,
            load_tx,
            load_rx,
            tab: Tab::Models,
            cfg,
            cfg_err,
            cfg_watcher: Watcher::new(path),
            dirty_at: None,
        };
        m.reload();
        m
    }

    fn reload(&mut self) {
        self.models = model::discover(&self.root);
        for m in &self.models {
            self.drafts
                .entry(m.toml_path.clone())
                .or_insert_with(|| Draft {
                    alias: m.cfg.alias.clone(),
                    args: m.cfg.args.clone(),
                });
        }
        self.drafts.retain(|k, _| self.models.iter().any(|m| &m.toml_path == k));
    }

    /// Index of the model the server is actually serving.
    fn loaded_index(&self) -> Option<usize> {
        let st = self.status.as_ref()?;
        let m = model::loaded(&self.models, st)?;
        self.models.iter().position(|x| x.toml_path == m.toml_path)
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if self.last_poll.elapsed() >= Duration::from_millis(1500) {
            self.last_poll = Instant::now();
            self.status = server::status();
            // Pick up hand edits, but never while the user is mid-change.
            if self.dirty_at.is_none() && self.cfg_watcher.changed() {
                match Config::load(&self.cfg_watcher.path) {
                    Ok(fresh) => {
                        self.cfg = fresh;
                        self.cfg_err = None;
                    }
                    Err(e) => self.cfg_err = Some(e),
                }
            }
        }
        while let Ok(result) = self.load_rx.try_recv() {
            match result {
                Ok(name) => self.note(format!("started {name}"), false),
                Err(e) => self.note(e, true),
            }
        }
        if self.message.is_some() && self.message_at.elapsed() > Duration::from_secs(6) {
            self.message = None;
        }
        if let Some(at) = self.dirty_at {
            if at.elapsed() >= Duration::from_millis(300) {
                self.dirty_at = None;
                if let Err(e) = self.cfg.save(&monitor_config::settings_path()) {
                    self.note(format!("settings save failed: {e}"), true);
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn note(&mut self, text: impl Into<String>, is_error: bool) {
        self.message = Some((text.into(), is_error));
        self.message_at = Instant::now();
    }

    fn save_draft(&mut self, m: &model::Model) {
        let Some(draft) = self.drafts.get(&m.toml_path) else {
            return;
        };
        let (alias, args) = (draft.alias.clone(), draft.args.clone());
        if let Err(e) = m.set_key("alias", &alias) {
            self.note(format!("save alias failed: {e}"), true);
            return;
        }
        if let Err(e) = m.set_key("args", &args) {
            self.note(format!("save args failed: {e}"), true);
            return;
        }
        self.note(format!("saved {}", m.cfg.name), false);
        self.reload();
    }

    fn spawn_monitor() -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = exe.parent().ok_or("no exe dir")?;
        let mon = dir.join("monitor.exe");
        let mut cmd = std::process::Command::new(&mon);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.spawn().map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn fmt_size(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    if bytes as f64 >= GB {
        format!("{:.2} GB", bytes as f64 / GB)
    } else {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    }
}

/// A coloured dot. egui's built-in font has no geometric-shape glyphs, so paint it.
fn dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

impl Manager {
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(err) = self.cfg_err.clone() {
            ui.colored_label(
                egui::Color32::from_rgb(230, 120, 120),
                format!("settings.toml is invalid: {err}"),
            );
            ui.label("It has not been overwritten. Fix it, or reset it to defaults.");
            if ui.button("Reset to defaults").clicked() {
                self.cfg = Config::defaults();
                match self.cfg.save(&monitor_config::settings_path()) {
                    Ok(()) => {
                        self.cfg_err = None;
                        self.note("settings reset", false);
                    }
                    Err(e) => self.note(format!("save failed: {e}"), true),
                }
            }
            ui.separator();
        }

        let enabled = self.cfg_err.is_none();
        let mut changed = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                ui.heading("Overlay");
                egui::Grid::new("layout_grid")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Width (0 = fit content)");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.cfg.monitor.width)
                                    .range(0.0..=1200.0)
                                    .speed(2.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Update interval (ms)");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.cfg.monitor.interval_ms)
                                    .range(250..=10_000)
                                    .speed(10.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Background opacity");
                        changed |= ui
                            .add(egui::Slider::new(
                                &mut self.cfg.monitor.background_alpha,
                                0..=255,
                            ))
                            .changed();
                        ui.end_row();

                        ui.label("Corner radius");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.cfg.monitor.corner_radius)
                                    .range(0.0..=40.0)
                                    .speed(0.5),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Font size");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.cfg.monitor.font_size)
                                    .range(8.0..=32.0)
                                    .speed(0.25),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Screen margin");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.cfg.monitor.margin)
                                    .range(0.0..=400.0)
                                    .speed(1.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Graph history (samples)");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.cfg.monitor.graph_samples)
                                    .range(1..=600)
                                    .speed(1.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Corner");
                        egui::ComboBox::from_id_salt("corner")
                            .selected_text(match self.cfg.monitor.position {
                                Corner::TopRight => "top-right",
                                Corner::TopLeft => "top-left",
                                Corner::BottomRight => "bottom-right",
                                Corner::BottomLeft => "bottom-left",
                            })
                            .show_ui(ui, |ui| {
                                for (value, text) in [
                                    (Corner::TopRight, "top-right"),
                                    (Corner::TopLeft, "top-left"),
                                    (Corner::BottomRight, "bottom-right"),
                                    (Corner::BottomLeft, "bottom-left"),
                                ] {
                                    changed |= ui
                                        .selectable_value(
                                            &mut self.cfg.monitor.position,
                                            value,
                                            text,
                                        )
                                        .changed();
                                }
                            });
                        ui.end_row();
                    });

                changed |= ui
                    .checkbox(&mut self.cfg.monitor.show_labels, "Show field labels")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.cfg.monitor.always_on_top, "Always on top")
                    .changed();
                changed |= ui
                    .checkbox(&mut self.cfg.monitor.show_close, "Show close button")
                    .changed();

                if ui.button("Reset position").clicked() {
                    monitor_config::clear_window_pos();
                    self.note("overlay position reset; it returns to the corner on next start", false);
                }

                ui.separator();
                ui.weak("Stored in:");
                ui.label(monitor_config::settings_path().display().to_string());
            });
        });

        if changed {
            self.dirty_at = Some(Instant::now());
        }
    }
}

impl eframe::App for Manager {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        let loaded = self.loaded_index();

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("QuantDock").strong().size(16.0));
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Models, "Models");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.separator();

                match &self.status {
                    Some(st) => {
                        let (name, alias) = match loaded {
                            Some(i) => (
                                self.models[i].cfg.name.clone(),
                                self.models[i].cfg.alias.clone(),
                            ),
                            None => ("unknown".to_owned(), st.served_id.clone()),
                        };
                        dot(ui, egui::Color32::from_rgb(90, 200, 120));
                        ui.label(format!("serving {alias} — {name}"));
                    }
                    None => {
                        dot(ui, egui::Color32::from_rgb(150, 150, 150));
                        ui.label("server down");
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Monitor").clicked() {
                        match Self::spawn_monitor() {
                            Ok(()) => self.note("monitor started", false),
                            Err(e) => self.note(format!("monitor failed: {e}"), true),
                        }
                    }
                    if ui.button("Refresh").clicked() {
                        self.reload();
                    }
                });
            });
        });

        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(self.root.display().to_string()).small().weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let running = self.status.is_some();
                    if ui
                        .add_enabled(
                            running,
                            egui::Button::new(if running { "Unload" } else { "Unloaded" }),
                        )
                        .clicked()
                    {
                        server::unload();
                        self.note("server stopped", false);
                    }
                });
            });
            if let Some((msg, is_err)) = &self.message {
                let color = if *is_err {
                    egui::Color32::from_rgb(230, 120, 120)
                } else {
                    egui::Color32::from_rgb(120, 200, 140)
                };
                ui.colored_label(color, msg);
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.tab == Tab::Settings {
                self.settings_ui(ui);
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for i in 0..self.models.len() {
                    let m = self.models[i].clone();
                    let is_loaded = loaded == Some(i);
                    let frame = egui::Frame::group(ui.style());
                    frame.show(ui, |ui| {
                        ui.set_width(ui.available_width());

                        ui.horizontal(|ui| {
                            if is_loaded {
                                dot(ui, egui::Color32::from_rgb(90, 200, 120));
                            }
                            ui.label(egui::RichText::new(&m.cfg.name).strong());
                            ui.label(
                                egui::RichText::new(format!("[{}]", m.cfg.alias)).weak().small(),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if is_loaded {
                                        ui.label(
                                            egui::RichText::new("RUNNING").color(
                                                egui::Color32::from_rgb(90, 200, 120),
                                            ),
                                        );
                                    } else if ui.button("Load").clicked() {
                                        // Stopping the old server takes ~2s: keep the UI alive.
                                        let m = m.clone();
                                        let tx = self.load_tx.clone();
                                        self.note(format!("starting {}", m.cfg.name), false);
                                        std::thread::spawn(move || {
                                            let result =
                                                server::load(&m).map(|()| m.cfg.name.clone());
                                            let _ = tx.send(result);
                                        });
                                    }
                                },
                            );
                        });

                        ui.label(
                            egui::RichText::new(format!(
                                "{}  •  {}",
                                m.gguf_path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                                fmt_size(m.size_bytes())
                            ))
                            .weak()
                            .small(),
                        );

                        let Some(draft) = self.drafts.get_mut(&m.toml_path) else {
                            return;
                        };
                        let dirty = draft.alias != m.cfg.alias || draft.args != m.cfg.args;

                        ui.horizontal(|ui| {
                            ui.label("alias");
                            ui.add(
                                egui::TextEdit::singleline(&mut draft.alias)
                                    .desired_width(120.0)
                                    .font(egui::TextStyle::Monospace),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("args");
                            ui.add(
                                egui::TextEdit::singleline(&mut draft.args)
                                    .desired_width(f32::INFINITY)
                                    .font(egui::TextStyle::Monospace),
                            );
                        });

                        if dirty {
                            ui.horizontal(|ui| {
                                if ui.button("Save to .toml").clicked() {
                                    self.save_draft(&m);
                                }
                                if ui.button("Revert").clicked() {
                                    self.drafts.insert(
                                        m.toml_path.clone(),
                                        Draft {
                                            alias: m.cfg.alias.clone(),
                                            args: m.cfg.args.clone(),
                                        },
                                    );
                                }
                            });
                        }
                    });
                }
            });
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("QuantDock")
            .with_inner_size([720.0, 620.0])
            .with_min_inner_size([520.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "QuantDock",
        options,
        Box::new(|_cc| Ok(Box::new(Manager::new()))),
    )
}
