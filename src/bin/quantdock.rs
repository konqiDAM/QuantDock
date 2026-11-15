// No console window for the released binary.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use quantdock::monitor_config::{self, Config, Corner, Style, Watcher};
use quantdock::monitor_sample::{Sample, Value};
use quantdock::{model, server};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LhmState {
    Unknown,
    Connected,
    InstalledNotRunning,
    Missing,
}

const LHM_ZIP: &str =
    "https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases/download/v0.9.6/LibreHardwareMonitor.zip";

/// Where an auto-downloaded copy lives. Per-user, so no elevation is needed to write it.
fn lhm_dir() -> PathBuf {
    monitor_config::data_dir().join("LibreHardwareMonitor")
}

fn lhm_exe(path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = path {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let candidate = lhm_dir().join("LibreHardwareMonitor.exe");
    candidate.is_file().then_some(candidate)
}

struct LhmProbe {
    state: LhmState,
    ids: Vec<String>,
}

fn lhm_probe(path: Option<&str>) -> LhmProbe {
    use std::sync::{Mutex, OnceLock};
    static READER: OnceLock<Mutex<quantdock::monitor_lhm::Reader>> = OnceLock::new();
    let reader = READER.get_or_init(|| Mutex::new(quantdock::monitor_lhm::Reader::new()));
    let mut guard = match reader.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    if let Some(data) = guard.read() {
        return LhmProbe {
            state: LhmState::Connected,
            ids: data.all.keys().cloned().collect(),
        };
    }
    drop(guard);
    LhmProbe {
        state: if lhm_exe(path).is_some() {
            LhmState::InstalledNotRunning
        } else {
            LhmState::Missing
        },
        ids: Vec::new(),
    }
}

fn wants_lhm_autostart(cfg: &Config) -> bool {
    cfg.sensors.enabled
        && cfg.sensors.autostart
        && cfg.monitor.rows.iter().any(|row| {
            row.fields.iter().any(|id| {
                quantdock::monitor_lhm::field_needs_lhm(id) || id.starts_with("lhm:")
            })
        })
}

#[derive(Clone)]
struct PickerSensor {
    id: String,
    text: String,
    unit: String,
    value: f64,
}

fn picker_list() -> Vec<PickerSensor> {
    let mut reader = quantdock::monitor_lhm::Reader::new();
    let Some(data) = reader.read() else {
        return Vec::new();
    };
    let mut list: Vec<PickerSensor> = data
        .all
        .iter()
        .map(|(id, v)| PickerSensor {
            id: id.clone(),
            text: v.text.clone(),
            unit: v.unit.clone(),
            value: v.value,
        })
        .collect();
    list.sort_by(|a, b| {
        a.text
            .to_lowercase()
            .cmp(&b.text.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    list
}

/// True when any row already shows `id`.
fn field_checked(cfg: &Config, id: &str) -> bool {
    cfg.monitor
        .rows
        .iter()
        .any(|row| row.fields.iter().any(|field| field == id))
}

/// Adds `id` to `row` unless any row already shows it. Returns whether it was added.
fn add_field_to_row(cfg: &mut Config, row: usize, id: &str) -> bool {
    if field_checked(cfg, id) {
        return false;
    }
    let Some(target) = cfg.monitor.rows.get_mut(row) else {
        return false;
    };
    target.fields.push(id.to_owned());
    true
}

/// Removes `id` from every row. Stored label/style/thresholds stay.
fn remove_field_everywhere(cfg: &mut Config, id: &str) -> bool {
    let mut removed = false;
    for row in &mut cfg.monitor.rows {
        let before = row.fields.len();
        row.fields.retain(|field| field != id);
        removed |= row.fields.len() != before;
    }
    removed
}

/// The only destructive custom action: drop the row entries and all stored config.
fn forget_custom_field(cfg: &mut Config, id: &str) {
    remove_field_everywhere(cfg, id);
    cfg.monitor.custom.remove(id);
    cfg.monitor.field_style.remove(id);
    cfg.thresholds.remove(id);
}

fn custom_label(text: &str) -> String {
    text.to_uppercase().chars().take(12).collect()
}

#[cfg(test)]
fn add_picked_sensor(cfg: &mut Config, row: usize, sensor: &PickerSensor) -> bool {
    if !add_field_to_row(cfg, row, &sensor.id) {
        return false;
    }
    let label = custom_label(&sensor.text);
    cfg.monitor.custom.entry(sensor.id.clone()).or_insert(label);
    true
}

fn is_missing(known: &[String], id: &str) -> bool {
    !known.iter().any(|k| k == id)
}

/// Downloads the pinned release, extracts it with the built-in `tar.exe`, and makes
/// sure the web server starts with the app.
fn lhm_download() -> Result<PathBuf, String> {
    let dir = lhm_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let zip = dir.join("libre-hardware-monitor.zip");

    let agent = ureq::config::Config::builder()
        .timeout_global(Some(Duration::from_secs(180)))
        .build()
        .new_agent();
    let mut resp = agent.get(LHM_ZIP).call().map_err(|e| e.to_string())?;
    let bytes = resp.body_mut().read_to_vec().map_err(|e| e.to_string())?;
    if bytes.len() < 1_000_000 {
        return Err("download looked truncated".to_owned());
    }
    std::fs::write(&zip, &bytes).map_err(|e| e.to_string())?;

    let status = quantdock::hidden(&mut std::process::Command::new("tar"))
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&dir)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    let _ = std::fs::remove_file(&zip);
    if !status.success() {
        return Err("extracting the archive failed".to_owned());
    }

    ensure_web_server(&dir)?;

    let exe = dir.join("LibreHardwareMonitor.exe");
    if !exe.is_file() {
        return Err("LibreHardwareMonitor.exe missing after extraction".to_owned());
    }
    Ok(exe)
}

/// Makes LibreHardwareMonitor start its web server on launch and stay out of the way,
/// so a fresh install works without menu clicks and without a window.
fn ensure_web_server(dir: &Path) -> Result<(), String> {
    let config = dir.join("LibreHardwareMonitor.config");
    let body = std::fs::read_to_string(&config).unwrap_or_else(|_| {
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<configuration>\n  <appSettings>\n  </appSettings>\n</configuration>\n".to_owned()
    });
    let mut patched = body;
    for key in ["runWebServerMenuItem", "startMinMenuItem", "minTrayMenuItem"] {
        if !patched.contains(key) {
            patched = patched.replace(
                "<appSettings>",
                &format!("<appSettings>\n    <add key=\"{key}\" value=\"true\" />"),
            );
        }
    }
    if patched != std::fs::read_to_string(&config).unwrap_or_default() {
        std::fs::write(&config, patched).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(windows)]
fn lhm_running() -> bool {
    use sysinfo::ProcessesToUpdate;
    let Ok(mut sys) = sysinfo::System::new() else {
        return false;
    };
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes().values().any(|p| {
        p.name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("librehardwaremonitor")
    })
}

#[cfg(not(windows))]
fn lhm_running() -> bool {
    false
}

/// Windows requires administrator for sensor access, so this shows a UAC prompt.
#[cfg(windows)]
fn lhm_start(path: &Path) -> Result<(), String> {
    if lhm_running() {
        // Starting a second copy would fight the first over the hardware.
        return Err(
            "already running — enable its web server from Options in its own window".to_owned(),
        );
    }
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            windows::core::w!("runas"),
            windows::core::PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if (result.0 as isize) <= 32 {
        Err("Windows refused to start it (UAC cancelled?)".to_owned())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn lhm_start(_path: &Path) -> Result<(), String> {
    Err("unsupported platform".to_owned())
}

impl Manager {
    /// Re-probe off the UI thread: a loopback request can take up to 900 ms.
    fn refresh_lhm_probe(&mut self) {
        self.lhm_state = LhmState::Unknown;
        self.lhm_ids.clear();
        self.last_probe = Instant::now();
        let tx = self.probe_tx.clone();
        let path = self.cfg.sensors.path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(lhm_probe(path.as_deref()));
        });
    }

    /// Optional LibreHardwareMonitor backend: discover, download, locate, start.
    fn sensors_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(
                    &mut self.cfg.sensors.enabled,
                    "Use LibreHardwareMonitor for CPU / board sensors",
                )
                .changed();
        });
        changed |= ui
            .checkbox(
                &mut self.cfg.sensors.autostart,
                "Start LibreHardwareMonitor with QuantDock",
            )
            .changed();

        match self.lhm_state {
            LhmState::Connected => {
                ui.horizontal(|ui| {
                    dot(ui, egui::Color32::from_rgb(90, 200, 120));
                    ui.label("connected — CPU temperature, CPU watts and board sensors are live");
                });
            }
            LhmState::InstalledNotRunning => {
                let path = lhm_exe(self.cfg.sensors.path.as_deref());
                ui.label("Found, but not running. Windows will ask for administrator.");
                ui.horizontal(|ui| {
                    if ui.button("Start it").clicked() {
                        match path.as_deref().map(lhm_start) {
                            Some(Ok(())) => self.note("starting LibreHardwareMonitor…", false),
                            Some(Err(e)) => self.note(e, true),
                            None => self.note("path not found", true),
                        }
                    }
                    if let Some(p) = &path {
                        ui.weak(p.display().to_string());
                    }
                });
            }
            LhmState::Missing => {
                ui.label("Not installed. Download it, or point at an existing copy.");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.lhm_busy, egui::Button::new("Download"))
                        .clicked()
                    {
                        self.lhm_busy = true;
                        let tx = self.sensors_tx.clone();
                        std::thread::spawn(move || {
                            let _ = tx.send(lhm_download().map(|p| p.display().to_string()));
                        });
                    }
                    if self.lhm_busy {
                        ui.spinner();
                        ui.weak("downloading…");
                    }
                    if ui.button("Cancel").clicked() {
                        // "Cancel" means: don't use sensors after all.
                        self.cfg.sensors.enabled = false;
                        changed = true;
                    }
                });
            }
            LhmState::Unknown => {
                ui.weak("checking…");
            }
        }

        ui.horizontal(|ui| {
            ui.label("Path");
            ui.add(
                egui::TextEdit::singleline(&mut self.lhm_path_input)
                    .hint_text("…\\LibreHardwareMonitor.exe")
                    .desired_width(320.0),
            );
            if ui.button("Use this path").clicked() {
                self.cfg.sensors.path = Some(self.lhm_path_input.clone());
                // An existing install gets its web server enabled too.
                if let Some(dir) = Path::new(&self.lhm_path_input).parent() {
                    if let Err(e) = ensure_web_server(dir) {
                        self.note(format!("could not enable the web server: {e}"), true);
                    }
                }
                self.refresh_lhm_probe();
                changed = true;
            }
        });

        changed
    }
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
    /// Unsaved settings edits.
    dirty: bool,
    disk_changed: bool,
    custom_id_input: String,
    monitor_up: bool,
    monitor_hover: bool,
    logs_open: bool,
    last_log: Option<(String, PathBuf)>,
    log_text: String,
    log_at: Instant,
    /// Model whose load was requested but is not serving yet.
    pending: Option<(PathBuf, Instant)>,
    sensors_tx: std::sync::mpsc::Sender<Result<String, String>>,
    sensors_rx: std::sync::mpsc::Receiver<Result<String, String>>,
    probe_tx: std::sync::mpsc::Sender<LhmProbe>,
    probe_rx: std::sync::mpsc::Receiver<LhmProbe>,
    last_probe: Instant,
    lhm_state: LhmState,
    lhm_ids: Vec<String>,
    lhm_autostart_done: bool,
    picker_open: bool,
    picker_sensors: Vec<PickerSensor>,
    picker_filter: String,
    picker_target: usize,
    picker_busy: bool,
    picker_tx: std::sync::mpsc::Sender<Vec<PickerSensor>>,
    picker_rx: std::sync::mpsc::Receiver<Vec<PickerSensor>>,
    lhm_busy: bool,
    lhm_path_input: String,
}

impl Manager {
    fn new() -> Self {
        let root = model::models_root();
        let (load_tx, load_rx) = std::sync::mpsc::channel();
        let (sensors_tx, sensors_rx) = std::sync::mpsc::channel();
        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        let (picker_tx, picker_rx) = std::sync::mpsc::channel();
        let path = monitor_config::settings_path();
        let (cfg, cfg_err) = match Config::load_existing(&path) {
            Ok(Some(c)) => (c, None),
            Ok(None) => {
                // First run: create the file. A broken one is never overwritten.
                let defaults = Config::defaults();
                let _ = defaults.save(&path);
                (defaults, None)
            }
            Err(e) => (Config::defaults(), Some(e)),
        };
        // Autostart is a launch-time decision; enabling it later is a manual start.
        let autostart_at_launch = cfg.sensors.enabled && cfg.sensors.autostart;
        let picker_target = cfg.monitor.rows.len().saturating_sub(1);
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
            dirty: false,
            disk_changed: false,
            custom_id_input: String::new(),
            monitor_up: false,
            monitor_hover: false,
            logs_open: false,
            last_log: None,
            log_text: String::new(),
            log_at: Instant::now() - Duration::from_secs(2),
            pending: None,
            sensors_tx,
            sensors_rx,
            probe_tx,
            probe_rx,
            last_probe: Instant::now() - Duration::from_secs(10),
            lhm_state: LhmState::Unknown,
            lhm_ids: Vec::new(),
            lhm_autostart_done: !autostart_at_launch,
            picker_open: false,
            picker_sensors: Vec::new(),
            picker_filter: String::new(),
            picker_target,
            picker_busy: false,
            picker_tx,
            picker_rx,
            lhm_busy: false,
            lhm_path_input: String::new(),
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
        // Probing talks HTTP, so it runs on a thread and reports back.
        if self.cfg.sensors.enabled && self.last_probe.elapsed() >= Duration::from_secs(5) {
            self.last_probe = Instant::now();
            let tx = self.probe_tx.clone();
            let path = self.cfg.sensors.path.clone();
            std::thread::spawn(move || {
                let _ = tx.send(lhm_probe(path.as_deref()));
            });
        }
        while let Ok(probe) = self.probe_rx.try_recv() {
            self.lhm_state = probe.state;
            self.lhm_ids = probe.ids;
        }
        while let Ok(list) = self.picker_rx.try_recv() {
            self.picker_busy = false;
            self.picker_sensors = list;
        }
        if !self.lhm_autostart_done && self.lhm_state != LhmState::Unknown {
            self.lhm_autostart_done = true;
            if wants_lhm_autostart(&self.cfg) && self.lhm_state == LhmState::InstalledNotRunning {
                match lhm_exe(self.cfg.sensors.path.as_deref()).as_deref().map(lhm_start) {
                    Some(Ok(())) => self.note("starting LibreHardwareMonitor…", false),
                    Some(Err(e)) => self.note(e, true),
                    None => self.note("LibreHardwareMonitor path not found", true),
                }
            }
        }
        if self.last_poll.elapsed() >= Duration::from_millis(1500) {
            self.last_poll = Instant::now();
            self.status = server::status();
            self.monitor_up = Self::monitor_running();
            if self.last_log.is_none() {
                if let Some(i) = self.loaded_index() {
                    let name = self.models[i].cfg.name.clone();
                    let path = server::log_path(&self.models[i]);
                    self.last_log = Some((name, path));
                }
            }
            // Pick up hand edits, but never while the user is mid-change.
            if self.cfg_watcher.changed() {
                if self.dirty {
                    self.disk_changed = true;
                } else {
                    match Config::load_existing(&self.cfg_watcher.path) {
                        Ok(Some(fresh)) => {
                            self.cfg = fresh;
                            self.cfg_err = None;
                        }
                        Ok(None) => {}
                        Err(e) => self.cfg_err = Some(e),
                    }
                }
            }
        }
        while let Ok(result) = self.load_rx.try_recv() {
            match result {
                Ok(name) => self.note(format!("started {name}"), false),
                Err(e) => {
                    self.pending = None;
                    self.note(e, true);
                }
            }
        }
        while let Ok(result) = self.sensors_rx.try_recv() {
            self.lhm_busy = false;
            match result {
                Ok(path) => {
                    self.cfg.sensors.path = Some(path.clone());
                    self.cfg.sensors.enabled = true;
                    self.lhm_path_input = path;
                    self.refresh_lhm_probe();
                    self.dirty = true;
                    self.note("LibreHardwareMonitor installed — press Start it", false);
                }
                Err(e) => self.note(format!("sensor download failed: {e}"), true),
            }
        }
        // A pending load stops being "pending" once it serves, or after a minute.
        if let Some((path, started)) = &self.pending {
            let path = path.clone();
            let stale = started.elapsed() > Duration::from_secs(60);
            let serving = self
                .loaded_index()
                .is_some_and(|i| self.models[i].toml_path == path);
            if stale || serving {
                self.pending = None;
            }
        }
        if self.message.is_some() && self.message_at.elapsed() > Duration::from_secs(6) {
            self.message = None;
        }
        ctx.request_repaint_after(Duration::from_millis(2000));
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

    /// The overlay owns a named mutex. Opening it (never creating it) tells us whether
    /// one is running.
    #[cfg(windows)]
    fn monitor_running() -> bool {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_ACCESS_RIGHTS};
        unsafe {
            // SYNCHRONIZE is the lowest right that still proves the object exists.
            match OpenMutexW(
                SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0000),
                false,
                windows::core::w!("Local\\QuantDockMonitor"),
            ) {
                Ok(handle) => {
                    let _ = CloseHandle(handle);
                    true
                }
                Err(_) => false,
            }
        }
    }

    #[cfg(not(windows))]
    fn monitor_running() -> bool {
        false
    }

    #[cfg(windows)]
    fn close_monitor() {
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
        unsafe {
            if let Ok(hwnd) = FindWindowW(None, windows::core::w!("QuantDock Monitor")) {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default());
            }
        }
    }

    #[cfg(not(windows))]
    fn close_monitor() {}
}

fn fmt_size(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    if bytes as f64 >= GB {
        format!("{:.2} GB", bytes as f64 / GB)
    } else {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    }
}

fn style_name(s: Style) -> &'static str {
    match s {
        Style::Text => "text",
        Style::Bar => "bar",
        Style::Graph => "graph",
        Style::GraphText => "graph+text",
    }
}

/// A coloured dot. egui's built-in font has no geometric-shape glyphs, so paint it.
fn dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Plausible unit for a custom sensor, so the preview is not a row of `--`.
fn preview_unit(id: &str) -> &'static str {
    if id.contains("/temperature/") {
        "°C"
    } else if id.contains("/fan/") {
        "RPM"
    } else if id.contains("/clock/") {
        "MHz"
    } else if id.contains("/voltage/") {
        "V"
    } else if id.contains("/power/") {
        "W"
    } else if id.contains("/load/") {
        "%"
    } else {
        ""
    }
}

/// Fixed sample for the preview: plausible values, with `cpu_peak` above its default
/// warn so threshold colours are visible without waiting for the GPU.
fn preview_sample(cfg: &Config) -> Sample {
    let mut s = Sample::default();
    let text = |s: &mut Sample, id: &'static str, v: &str| {
        s.fields.insert(id, Value::Text(v.to_owned()));
    };
    text(&mut s, "model", "Qwen3.8-27B-R3-mix");
    text(&mut s, "alias", "local");
    text(&mut s, "ctx", "262K");
    text(&mut s, "pstate", "P0");
    text(&mut s, "uptime", "01:24");
    text(&mut s, "port", "8080");

    let kb = 1024.0;
    s.fields.insert("gpu", Value::Num(15.0));
    s.fields.insert("vram", Value::UsedTotal(3.2 * kb, 24.0 * kb));
    s.fields.insert("vram_pct", Value::Num(13.0));
    s.fields.insert("vram_bus", Value::Num(6.0));
    s.fields.insert("gpu_temp", Value::Num(52.0));
    s.fields.insert("fan", Value::Num(61.0));
    s.fields.insert("power", Value::Num(112.0));
    s.fields.insert("clock", Value::Num(225.0));
    s.fields.insert("cpu", Value::Num(34.0));
    s.fields.insert("cpu_peak", Value::Num(92.0));
    s.fields.insert("cpu_hot", Value::Num(3.0));
    s.fields.insert("ram", Value::UsedTotal(12.4 * kb, 64.0 * kb));
    s.fields.insert("ram_pct", Value::Num(19.0));
    s.fields.insert("swap", Value::UsedTotal(0.0, 8.0 * kb));
    s.fields.insert("llama_ram", Value::Num(812.0));
    s.fields.insert("disk_free", Value::Num(431.0 * kb));
    // Hardware sensors, so the default rows are not all "--" in the preview.
    s.fields.insert("cpu_temp", Value::Num(48.0));
    s.fields.insert("cpu_power", Value::Num(74.0));
    s.fields.insert("board_temp", Value::Num(42.0));
    s.fields.insert("gpu_hotspot", Value::Num(72.0));
    s.fields.insert("gpu_mem_temp", Value::Num(66.0));
    s.fields.insert("gpu_shared", Value::Num(222.0));
    s.fields.insert("fan_rpm", Value::Num(1243.0));

    for (id, label) in &cfg.monitor.custom {
        s.custom.insert(
            id.clone(),
            quantdock::monitor_lhm::SensorValue {
                text: label.clone(),
                unit: preview_unit(id).to_owned(),
                value: 42.0,
            },
        );
    }

    // A plausible load curve so graph styles have something to draw.
    let n = cfg.monitor.graph_samples.max(2);
    for id in [
        "gpu", "cpu", "cpu_peak", "vram_pct", "ram_pct", "fan", "gpu_temp", "power", "vram_bus",
    ] {
        let base = match id {
            "gpu" => 15.0,
            "cpu" => 34.0,
            "cpu_peak" => 78.0,
            _ => 40.0,
        };
        let hist = (0..n)
            .map(|k| base + 12.0 * (k as f64 / n as f64 * std::f64::consts::TAU).sin())
            .collect();
        s.history.insert(id, hist);
    }
    s
}

impl Manager {
    /// Row builder: chips per row, with add/remove/reorder.
    fn rows_ui(&mut self, ui: &mut egui::Ui) -> bool {
        use quantdock::monitor_config::{field_def, FIELDS};

        enum Act {
            RemoveField(usize, usize),
            MoveField(usize, usize, isize),
            AddField(usize, String),
            DeleteRow(usize),
            MoveRow(usize, isize),
            AddRow,
        }

        let mut actions: Vec<Act> = Vec::new();
        let row_count = self.cfg.monitor.rows.len();

        for i in 0..row_count {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format!("Row {}", i + 1)).weak());
                let fields = self.cfg.monitor.rows[i].fields.clone();
                if fields.is_empty() {
                    ui.weak("(spacer)");
                }
                for (j, id) in fields.iter().enumerate() {
                    let label = field_def(id)
                        .map(|d| if d.label.is_empty() { d.id } else { d.label })
                        .unwrap_or(id.as_str());
                    if ui
                        .small_button(format!("{label} x"))
                        .on_hover_text(id)
                        .clicked()
                    {
                        actions.push(Act::RemoveField(i, j));
                    }
                    if ui.small_button("<").on_hover_text("move left").clicked() {
                        actions.push(Act::MoveField(i, j, -1));
                    }
                    if ui.small_button(">").on_hover_text("move right").clicked() {
                        actions.push(Act::MoveField(i, j, 1));
                    }
                }
                ui.menu_button("+ field", |ui| {
                    for def in FIELDS {
                        if field_checked(&self.cfg, def.id) {
                            continue;
                        }
                        if ui.button(def.id).clicked() {
                            actions.push(Act::AddField(i, def.id.to_string()));
                            ui.close();
                        }
                    }
                });
                if ui.small_button("Up").clicked() {
                    actions.push(Act::MoveRow(i, -1));
                }
                if ui.small_button("Down").clicked() {
                    actions.push(Act::MoveRow(i, 1));
                }
                if ui.small_button("Delete").clicked() {
                    actions.push(Act::DeleteRow(i));
                }
            });
        }

        if ui.button("+ Row").clicked() {
            actions.push(Act::AddRow);
        }

        let mut changed = false;
        for action in actions {
            match action {
                Act::RemoveField(i, j) => {
                    if let Some(row) = self.cfg.monitor.rows.get_mut(i) {
                        if j < row.fields.len() {
                            row.fields.remove(j);
                        }
                    }
                }
                Act::MoveField(i, j, delta) => {
                    if let Some(row) = self.cfg.monitor.rows.get_mut(i) {
                        let target = j as isize + delta;
                        if target >= 0 && (target as usize) < row.fields.len() {
                            row.fields.swap(j, target as usize);
                        }
                    }
                }
                Act::AddField(i, id) => {
                    add_field_to_row(&mut self.cfg, i, &id);
                }
                Act::DeleteRow(i) => {
                    if i < self.cfg.monitor.rows.len() {
                        self.cfg.monitor.rows.remove(i);
                    }
                }
                Act::MoveRow(i, delta) => {
                    let target = i as isize + delta;
                    if target >= 0 && (target as usize) < self.cfg.monitor.rows.len() {
                        self.cfg.monitor.rows.swap(i, target as usize);
                    }
                }
                Act::AddRow => self.cfg.monitor.rows.push(Default::default()),
            }
            changed = true;
        }
        changed
    }

    /// One line per field in use: display style, warn, crit, direction.
    fn fields_ui(&mut self, ui: &mut egui::Ui) -> bool {
        use quantdock::monitor_config::{field_def, Direction, FieldKind, Threshold, FIELDS};

        let mut used: Vec<&'static str> = Vec::new();
        for row in &self.cfg.monitor.rows {
            for id in &row.fields {
                if let Some(def) = field_def(id) {
                    if !used.contains(&def.id) {
                        used.push(def.id);
                    }
                }
            }
        }
        // Keep the vocabulary's order so the table does not jump around.
        used.sort_by_key(|id| FIELDS.iter().position(|d| d.id == *id).unwrap_or(usize::MAX));

        struct State {
            id: &'static str,
            style: Style,
            threshold: Threshold,
        }
        let before: Vec<(Style, Threshold)> = used
            .iter()
            .map(|id| {
                (
                    self.cfg.monitor.field_style.get(*id).copied().unwrap_or_default(),
                    self.cfg.thresholds.get(*id).cloned().unwrap_or_default(),
                )
            })
            .collect();
        let mut states: Vec<State> = used
            .iter()
            .zip(before.iter())
            .map(|(id, (style, threshold))| State {
                id,
                style: *style,
                threshold: threshold.clone(),
            })
            .collect();
        let sensors_off = self.lhm_state != LhmState::Connected;

        egui::Grid::new("fields_grid")
            .num_columns(5)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                ui.strong("Field");
                ui.strong("Style");
                ui.strong("Warn");
                ui.strong("Crit");
                ui.strong("Direction");
                ui.end_row();

                for state in states.iter_mut() {
                    let Some(def) = field_def(state.id) else { continue };
                    let mut name = if def.label.is_empty() {
                        def.id.to_owned()
                    } else {
                        format!("{} ({})", def.label, def.id)
                    };
                    if quantdock::monitor_lhm::field_needs_lhm(def.id) && sensors_off {
                        name.push_str(" — needs sensors");
                    }
                    ui.label(name);

                    if matches!(def.kind, FieldKind::Text) {
                        ui.weak("text");
                    } else {
                        egui::ComboBox::from_id_salt(("style", state.id))
                            .selected_text(style_name(state.style))
                            .width(90.0)
                            .show_ui(ui, |ui| {
                                for s in [Style::Text, Style::Bar, Style::Graph, Style::GraphText] {
                                    ui.selectable_value(&mut state.style, s, style_name(s));
                                }
                            });
                    }

                    for slot in [&mut state.threshold.warn, &mut state.threshold.crit] {
                        ui.horizontal(|ui| {
                            let mut on = slot.is_some();
                            if ui.checkbox(&mut on, "").changed() {
                                *slot = if on { Some(0.0) } else { None };
                            }
                            match slot {
                                Some(v) => {
                                    ui.add(egui::DragValue::new(v).speed(1.0));
                                }
                                None => {
                                    ui.weak("--");
                                }
                            }
                        });
                    }

                    egui::ComboBox::from_id_salt(("dir", state.id))
                        .selected_text(match state.threshold.direction {
                            Direction::Above => "above",
                            Direction::Below => "below",
                        })
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.threshold.direction,
                                Direction::Above,
                                "above",
                            );
                            ui.selectable_value(
                                &mut state.threshold.direction,
                                Direction::Below,
                                "below",
                            );
                        });
                    ui.end_row();
                }
            });

        let mut changed = false;
        for (state, (old_style, old_threshold)) in states.iter().zip(before.iter()) {
            if state.style != *old_style {
                if state.style == Style::Text {
                    self.cfg.monitor.field_style.remove(state.id);
                } else {
                    self.cfg
                        .monitor
                        .field_style
                        .insert(state.id.to_owned(), state.style);
                }
                changed = true;
            }
            if state.threshold.warn != old_threshold.warn
                || state.threshold.crit != old_threshold.crit
                || state.threshold.direction != old_threshold.direction
            {
                if state.threshold.warn.is_none() && state.threshold.crit.is_none() {
                    self.cfg.thresholds.remove(state.id);
                } else {
                    self.cfg
                        .thresholds
                        .insert(state.id.to_owned(), state.threshold.clone());
                }
                changed = true;
            }
        }
        changed
    }

    /// Label, style and thresholds for every stored custom sensor, checked or not.
    fn custom_fields_ui(&mut self, ui: &mut egui::Ui) -> bool {
        use quantdock::monitor_config::{Direction, Threshold};

        if self.cfg.monitor.custom.is_empty() {
            ui.weak("No custom sensors stored yet.");
            return false;
        }

        let mut ids: Vec<(String, String)> = self
            .cfg
            .monitor
            .custom
            .iter()
            .map(|(id, label)| (id.clone(), label.clone()))
            .collect();
        ids.sort_by(|a, b| {
            a.1.to_lowercase()
                .cmp(&b.1.to_lowercase())
                .then(a.0.cmp(&b.0))
        });

        struct State {
            id: String,
            label: String,
            style: Style,
            threshold: Threshold,
        }
        let mut states: Vec<State> = ids
            .into_iter()
            .map(|(id, label)| State {
                // Only a /load/ bar is meaningful; show text for every other stored style
                // without rewriting the config just because Settings was opened.
                style: match self.cfg.monitor.field_style.get(&id).copied() {
                    Some(Style::Bar) if id.contains("/load/") => Style::Bar,
                    _ => Style::Text,
                },
                threshold: self.cfg.thresholds.get(&id).cloned().unwrap_or_default(),
                id,
                label,
            })
            .collect();
        let before: Vec<(String, Style, Threshold)> = states
            .iter()
            .map(|s| (s.label.clone(), s.style, s.threshold.clone()))
            .collect();
        let mut forget: Option<String> = None;

        egui::Grid::new("custom_fields_grid")
            .num_columns(6)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                ui.strong("Field");
                ui.strong("Style");
                ui.strong("Warn");
                ui.strong("Crit");
                ui.strong("Direction");
                ui.label("");
                ui.end_row();

                for state in states.iter_mut() {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut state.label)
                                .desired_width(120.0),
                        )
                        .on_hover_text(&state.id);
                        if self.lhm_state == LhmState::Connected
                            && is_missing(&self.lhm_ids, &state.id)
                        {
                            ui.colored_label(
                                egui::Color32::from_rgb(230, 190, 80),
                                "(missing)",
                            );
                        }
                    });

                    if state.id.contains("/load/") {
                        egui::ComboBox::from_id_salt(("custom_style", &state.id))
                            .selected_text(style_name(state.style))
                            .width(90.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut state.style, Style::Text, "text");
                                ui.selectable_value(&mut state.style, Style::Bar, "bar");
                            });
                    } else {
                        ui.weak("text");
                    }

                    for slot in [&mut state.threshold.warn, &mut state.threshold.crit] {
                        ui.horizontal(|ui| {
                            let mut on = slot.is_some();
                            if ui.checkbox(&mut on, "").changed() {
                                *slot = if on { Some(0.0) } else { None };
                            }
                            match slot {
                                Some(v) => {
                                    ui.add(egui::DragValue::new(v).speed(1.0));
                                }
                                None => {
                                    ui.weak("--");
                                }
                            }
                        });
                    }

                    egui::ComboBox::from_id_salt(("custom_dir", &state.id))
                        .selected_text(match state.threshold.direction {
                            Direction::Above => "above",
                            Direction::Below => "below",
                        })
                        .width(80.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.threshold.direction,
                                Direction::Above,
                                "above",
                            );
                            ui.selectable_value(
                                &mut state.threshold.direction,
                                Direction::Below,
                                "below",
                            );
                        });

                    if ui.button("Forget").clicked() {
                        forget = Some(state.id.clone());
                    }
                    ui.end_row();
                }
            });

        let mut changed = false;
        for (state, before) in states.iter().zip(before.iter()) {
            if state.label != before.0 {
                self.cfg
                    .monitor
                    .custom
                    .insert(state.id.clone(), state.label.clone());
                changed = true;
            }
            if state.style != before.1 {
                if state.style == Style::Text {
                    self.cfg.monitor.field_style.remove(&state.id);
                } else {
                    self.cfg
                        .monitor
                        .field_style
                        .insert(state.id.clone(), state.style);
                }
                changed = true;
            }
            if state.threshold.warn != before.2.warn
                || state.threshold.crit != before.2.crit
                || state.threshold.direction != before.2.direction
            {
                if state.threshold.warn.is_none() && state.threshold.crit.is_none() {
                    self.cfg.thresholds.remove(&state.id);
                } else {
                    self.cfg
                        .thresholds
                        .insert(state.id.clone(), state.threshold.clone());
                }
                changed = true;
            }
        }
        if let Some(id) = forget {
            forget_custom_field(&mut self.cfg, &id);
            changed = true;
        }
        changed
    }

    /// Renders the real panel with a fixed sample so layout and colours are visible.
    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let sample = preview_sample(&self.cfg);
        let size = self.cfg.monitor.font_size;
        let measure = |text: &str, size: f32| {
            ui.painter()
                .layout_no_wrap(
                    text.to_owned(),
                    egui::FontId::proportional(size),
                    egui::Color32::WHITE,
                )
                .size()
                .x
        };
        let natural = quantdock::monitor_panel::natural_width(&self.cfg, &sample, &measure);
        let width = if self.cfg.monitor.width > 0.0 {
            self.cfg.monitor.width
        } else {
            natural.max(320.0)
        };
        let rows = self.cfg.monitor.rows.len().max(1) as f32;
        let height = rows * (size * 2.0) + 28.0;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        quantdock::monitor_panel::render(&mut child, &self.cfg, &sample);
    }

    fn theme_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            let mut swatch = |ui: &mut egui::Ui, label: &str, color: &mut egui::Color32| {
                ui.label(label);
                changed |= egui::color_picker::color_edit_button_srgba(
                    ui,
                    color,
                    egui::color_picker::Alpha::Opaque,
                )
                .changed();
                ui.add_space(8.0);
            };
            swatch(ui, "text", &mut self.cfg.theme.text);
            swatch(ui, "label", &mut self.cfg.theme.label);
            swatch(ui, "accent", &mut self.cfg.theme.accent);
            swatch(ui, "warn", &mut self.cfg.theme.warn);
            swatch(ui, "crit", &mut self.cfg.theme.crit);
            swatch(ui, "background", &mut self.cfg.theme.background);
        });
        changed
    }

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
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.dirty, egui::Button::new("Save"))
                .clicked()
            {
                match self.cfg.save(&monitor_config::settings_path()) {
                    Ok(()) => {
                        self.dirty = false;
                        self.disk_changed = false;
                        self.note("settings saved", false);
                    }
                    Err(e) => self.note(format!("save failed: {e}"), true),
                }
            }
            if ui.button("Return").clicked() {
                self.picker_open = false;
                match Config::load_existing(&monitor_config::settings_path()) {
                    Ok(Some(fresh)) => {
                        self.cfg = fresh;
                        self.cfg_err = None;
                        self.dirty = false;
                        self.disk_changed = false;
                        self.tab = Tab::Models;
                    }
                    Ok(None) => {
                        self.dirty = false;
                        self.disk_changed = false;
                        self.tab = Tab::Models;
                    }
                    Err(e) => {
                        // Keep the draft and the notice: Return did not reload anything.
                        self.cfg_err = Some(e);
                    }
                }
            }
            if self.dirty {
                ui.colored_label(egui::Color32::from_rgb(230, 190, 80), "unsaved changes");
            }
            if self.disk_changed {
                ui.colored_label(
                    egui::Color32::from_rgb(230, 190, 80),
                    "settings.toml changed on disk — Return reloads it, Save overwrites it.",
                );
            }
        });
        ui.separator();
        let mut changed = false;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                ui.heading("Overlay");
                egui::Grid::new("layout_grid")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Width (px; 0 = fit content)");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut self.cfg.monitor.width)
                                    .range(0.0..=1200.0)
                                    .speed(2.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("Title");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut self.cfg.monitor.title)
                                    .hint_text("shown above the rows")
                                    .desired_width(240.0),
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
                ui.heading("Hardware sensors");
                ui.weak("Optional. Adds CPU temperature, CPU watts and board sensors.");
                changed |= self.sensors_ui(ui);

                ui.separator();
                ui.heading("Rows");
                ui.weak("Each row is a line of fields, left to right. An empty row is a spacer.");
                changed |= self.rows_ui(ui);

                ui.separator();
                ui.heading("Fields");
                ui.weak("Style, warn and crit per field. Warn/crit are off until ticked.");
                ui.horizontal(|ui| {
                    if ui
                        .add_sized(
                            [240.0, 30.0],
                            egui::Button::new(
                                egui::RichText::new("Configure available sensors…").strong(),
                            ),
                        )
                        .clicked()
                    {
                        self.picker_open = true;
                        self.start_picker_fetch();
                    }
                    let checked: usize = self
                        .cfg
                        .monitor
                        .rows
                        .iter()
                        .map(|row| row.fields.len())
                        .sum();
                    ui.weak(format!(
                        "{checked} checked · {} LHM sensors",
                        self.picker_sensors.len()
                    ));
                });
                changed |= self.fields_ui(ui);

                ui.separator();
                ui.heading("Custom sensor fields");
                ui.weak("Every stored custom sensor, checked or not. Forget deletes its stored config.");
                changed |= self.custom_fields_ui(ui);

                ui.horizontal(|ui| {
                    ui.label("Add by SensorId");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.custom_id_input)
                            .hint_text("/nvidiagpu/0/clock/1")
                            .desired_width(260.0),
                    );
                    if ui.button("Add").clicked() {
                        let id = self
                            .custom_id_input
                            .trim()
                            .trim_start_matches("lhm:")
                            .to_owned();
                        if id.starts_with('/') {
                            let full = format!("lhm:{id}");
                            let label =
                                custom_label(id.rsplit('/').next().unwrap_or("sensor"));
                            self.cfg
                                .monitor
                                .custom
                                .entry(full.clone())
                                .or_insert(label);
                            if !self.cfg.monitor.rows.is_empty() {
                                let last = self.cfg.monitor.rows.len() - 1;
                                add_field_to_row(&mut self.cfg, last, &full);
                            }
                            self.custom_id_input.clear();
                            changed = true;
                        }
                    }
                });
                ui.weak("Use this for a SensorId LibreHardwareMonitor is not currently reporting.");

                ui.separator();
                ui.heading("Colours");
                changed |= self.theme_ui(ui);

                ui.separator();
                ui.heading("Preview");
                ui.weak("Actual overlay width; custom sensors use sample values.");
                self.preview_ui(ui);

                ui.separator();
                ui.weak("Stored in:");
                ui.label(monitor_config::settings_path().display().to_string());
            });
        });

        if changed {
            self.dirty = true;
        }
    }

    fn start_picker_fetch(&mut self) {
        if self.picker_busy {
            return;
        }
        self.picker_busy = true;
        let tx = self.picker_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(picker_list());
        });
    }

    /// The complete field list: built-ins plus every live or stored LHM sensor.
    fn sensor_window_ui(&mut self, ctx: &egui::Context) {
        use quantdock::monitor_config::FIELDS;

        if !self.picker_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Configure available sensors")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([900.0, 600.0])
            .min_width(640.0)
            .show(ctx, |ui| {
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label("Search");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.picker_filter)
                            .hint_text("GPU temperature, fan, voltage…")
                            .desired_width(300.0),
                    );
                    if ui.button("Refresh").clicked() {
                        self.start_picker_fetch();
                    }
                    if self.cfg.monitor.rows.is_empty() {
                        ui.weak("Add a row first.");
                    } else {
                        self.picker_target =
                            self.picker_target.min(self.cfg.monitor.rows.len() - 1);
                        egui::ComboBox::from_id_salt("sensor_target_row")
                            .selected_text(format!("Add to: Row {}", self.picker_target + 1))
                            .show_ui(ui, |ui| {
                                for i in 0..self.cfg.monitor.rows.len() {
                                    ui.selectable_value(
                                        &mut self.picker_target,
                                        i,
                                        format!("Row {}", i + 1),
                                    );
                                }
                            });
                    }
                });
                let checked: usize = self
                    .cfg
                    .monitor
                    .rows
                    .iter()
                    .map(|row| row.fields.len())
                    .sum();
                ui.weak(format!(
                    "{checked} checked · {} live LHM sensors",
                    self.picker_sensors.len()
                ));
                ui.separator();
                if self.picker_busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("reading sensors…");
                    });
                }

                let needle = self.picker_filter.to_lowercase();
                let mut toggle: Option<(String, bool)> = None;
                egui::ScrollArea::vertical()
                    .max_height(470.0)
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Built-in fields").strong());
                        for def in FIELDS {
                            let name = if def.label.is_empty() {
                                def.id
                            } else {
                                def.label
                            };
                            if !needle.is_empty()
                                && !name.to_lowercase().contains(&needle)
                                && !def.id.to_lowercase().contains(&needle)
                            {
                                continue;
                            }
                            let text = if def.label.is_empty() {
                                def.id.to_owned()
                            } else {
                                format!("{name}  ({})", def.id)
                            };
                            let mut on = field_checked(&self.cfg, def.id);
                            let can = on || !self.cfg.monitor.rows.is_empty();
                            if ui
                                .add_enabled(can, egui::Checkbox::new(&mut on, text))
                                .changed()
                            {
                                toggle = Some((def.id.to_owned(), on));
                            }
                        }

                        ui.separator();
                        ui.label(egui::RichText::new("LibreHardwareMonitor").strong());
                        if self.picker_sensors.is_empty() && !self.picker_busy {
                            ui.weak("Start LibreHardwareMonitor, then Refresh.");
                        }
                        for sensor in &self.picker_sensors {
                            if !needle.is_empty()
                                && !sensor.text.to_lowercase().contains(&needle)
                                && !sensor.id.to_lowercase().contains(&needle)
                            {
                                continue;
                            }
                            let mut on = field_checked(&self.cfg, &sensor.id);
                            let can = on || !self.cfg.monitor.rows.is_empty();
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        can,
                                        egui::Checkbox::new(&mut on, &sensor.text),
                                    )
                                    .on_hover_text(&sensor.id)
                                    .changed()
                                {
                                    toggle = Some((sensor.id.clone(), on));
                                }
                                ui.weak(format!("{} {}", sensor.value, sensor.unit));
                            });
                        }

                        let missing: Vec<(String, String)> = self
                            .cfg
                            .monitor
                            .custom
                            .iter()
                            .filter(|(id, _)| {
                                !self.picker_sensors.iter().any(|s| &s.id == *id)
                            })
                            .map(|(id, label)| (id.clone(), label.clone()))
                            .collect();
                        if !missing.is_empty() {
                            ui.separator();
                            ui.label(egui::RichText::new("Stored / missing").strong());
                            for (id, label) in missing {
                                if !needle.is_empty()
                                    && !label.to_lowercase().contains(&needle)
                                    && !id.to_lowercase().contains(&needle)
                                {
                                    continue;
                                }
                                let mut on = field_checked(&self.cfg, &id);
                                let can = on || !self.cfg.monitor.rows.is_empty();
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(
                                            can,
                                            egui::Checkbox::new(&mut on, &label),
                                        )
                                        .on_hover_text(&id)
                                        .changed()
                                    {
                                        toggle = Some((id.clone(), on));
                                    }
                                    ui.colored_label(
                                        egui::Color32::from_rgb(230, 190, 80),
                                        "(missing)",
                                    );
                                });
                            }
                        }
                    });

                if let Some((id, on)) = toggle {
                    if on {
                        let row = self
                            .picker_target
                            .min(self.cfg.monitor.rows.len().saturating_sub(1));
                        if add_field_to_row(&mut self.cfg, row, &id) {
                            if id.starts_with("lhm:")
                                && !self.cfg.monitor.custom.contains_key(&id)
                            {
                                let label = self
                                    .picker_sensors
                                    .iter()
                                    .find(|s| s.id == id)
                                    .map(|s| custom_label(&s.text))
                                    .unwrap_or_else(|| {
                                        custom_label(
                                            id.rsplit('/').next().unwrap_or("sensor"),
                                        )
                                    });
                                self.cfg.monitor.custom.insert(id.clone(), label);
                            }
                            changed = true;
                        }
                    } else if remove_field_everywhere(&mut self.cfg, &id) {
                        changed = true;
                    }
                }
                if changed {
                    self.dirty = true;
                }
            });
        self.picker_open = open;
    }

    fn refresh_log(&mut self) {
        let Some((_, path)) = &self.last_log else {
            self.log_text.clear();
            return;
        };
        if self.log_at.elapsed() < Duration::from_millis(1000) {
            return;
        }
        self.log_at = Instant::now();
        self.log_text = server::tail(path, 200).join("\n");
    }

    fn log_ui(&mut self, ui: &mut egui::Ui) {
        let (name, path) = match &self.last_log {
            Some((name, path)) => (name.clone(), path.clone()),
            None => (String::new(), PathBuf::new()),
        };

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Logs").strong());
            if self.last_log.is_some() {
                ui.weak(format!("— {name}"));
                ui.weak(path.display().to_string());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.logs_open = false;
                }
                if self.last_log.is_some() {
                    if ui.button("Copy path").clicked() {
                        ui.ctx().copy_text(path.display().to_string());
                    }
                    if ui.button("Open in editor").clicked() {
                        let _ = quantdock::hidden(
                            std::process::Command::new("cmd")
                                .args(["/C", "start", ""])
                                .arg(&path),
                        )
                        .spawn();
                    }
                    if ui.button("Refresh").clicked() {
                        self.log_at = Instant::now() - Duration::from_secs(2);
                    }
                }
            });
        });
        ui.separator();

        if self.last_log.is_none() {
            ui.weak("No model loaded — load a model to see its server log.");
            return;
        }
        if self.log_text.is_empty() {
            ui.weak("No log yet for this model.");
            return;
        }
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&self.log_text).monospace())
                        .selectable(true),
                );
            });
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
                ui.selectable_value(&mut self.tab, Tab::Settings, if self.dirty { "Settings *" } else { "Settings" });
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
                    let (label, fill) = if !self.monitor_up {
                        ("Monitor", ui.visuals().widgets.inactive.bg_fill)
                    } else if self.monitor_hover {
                        ("Close", egui::Color32::from_rgb(150, 52, 52))
                    } else {
                        ("Monitor on", egui::Color32::from_rgb(46, 120, 74))
                    };
                    // One size for every label, so hovering cannot resize the button.
                    let widest = ["Monitor", "Monitor on", "Close"]
                        .iter()
                        .map(|text| {
                            ui.painter()
                                .layout_no_wrap(
                                    (*text).to_owned(),
                                    egui::FontId::proportional(14.0),
                                    egui::Color32::WHITE,
                                )
                                .size()
                                .x
                        })
                        .fold(0.0f32, f32::max);
                    let button = ui.add(
                        egui::Button::new(
                            egui::RichText::new(label)
                                .color(egui::Color32::from_rgb(235, 235, 235)),
                        )
                        .fill(fill)
                        .min_size(egui::vec2(widest + 26.0, 0.0)),
                    );
                    self.monitor_hover = button.hovered();
                    if button.clicked() {
                        if self.monitor_up {
                            Self::close_monitor();
                            self.monitor_up = false;
                            self.note("monitor closed", false);
                        } else {
                            match Self::spawn_monitor() {
                                Ok(()) => self.note("monitor started", false),
                                Err(e) => self.note(format!("monitor failed: {e}"), true),
                            }
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
                    if ui.selectable_label(self.logs_open, "Logs").clicked() {
                        self.logs_open = !self.logs_open;
                        self.log_at = Instant::now() - Duration::from_secs(2);
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

        if self.logs_open {
            self.refresh_log();
            egui::Panel::bottom("logs")
                .resizable(true)
                .default_size(180.0)
                .min_size(80.0)
                .max_size(ui.ctx().content_rect().height() * 0.5)
                .show(ui, |ui| self.log_ui(ui));
        }

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
                    let frame = egui::Frame::group(ui.style()).stroke(if is_loaded {
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(90, 200, 120))
                    } else if self.pending.as_ref().map(|(p, _)| p) == Some(&m.toml_path) {
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(230, 190, 80))
                    } else {
                        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(18))
                    });
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
                                        self.pending =
                                            Some((m.toml_path.clone(), Instant::now()));
                                        self.last_log =
                                            Some((m.cfg.name.clone(), server::log_path(&m)));
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

        self.sensor_window_ui(&ctx);
    }
}

fn main() -> eframe::Result {
    quantdock::install_panic_log();
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

#[cfg(test)]
mod tests {
    use super::*;
    use quantdock::monitor_config::{Direction, Row, Style, Threshold};

    #[test]
    fn autostart_requires_enabled_autostart_and_an_lhm_field() {
        let mut cfg = Config::defaults();
        cfg.sensors.enabled = true;
        cfg.sensors.autostart = true;
        cfg.monitor.rows = vec![Row {
            fields: vec!["gpu_hotspot".into()],
        }];
        assert!(wants_lhm_autostart(&cfg));
    }

    #[test]
    fn autostart_requires_the_master_switch() {
        let mut cfg = Config::defaults();
        cfg.sensors.enabled = false;
        cfg.sensors.autostart = true;
        assert!(!wants_lhm_autostart(&cfg));
    }

    #[test]
    fn autostart_ignored_without_lhm_fields() {
        let mut cfg = Config::defaults();
        cfg.sensors.enabled = true;
        cfg.sensors.autostart = true;
        cfg.monitor.rows = vec![Row {
            fields: vec!["gpu".into(), "ram".into()],
        }];
        assert!(!wants_lhm_autostart(&cfg));
    }

    #[test]
    fn autostart_accepts_a_custom_sensor() {
        let mut cfg = Config::defaults();
        cfg.sensors.enabled = true;
        cfg.sensors.autostart = true;
        cfg.monitor.rows = vec![Row {
            fields: vec!["lhm:/gpu/0/temperature/0".into()],
        }];
        assert!(wants_lhm_autostart(&cfg));
    }

    fn sample_sensor() -> PickerSensor {
        PickerSensor {
            id: "lhm:/gpu/0/temperature/0".into(),
            text: "GPU Core".into(),
            unit: "°C".into(),
            value: 40.0,
        }
    }

    #[test]
    fn picker_add_does_not_duplicate_a_field_across_rows() {
        let mut cfg = Config::defaults();
        let sensor = sample_sensor();
        assert!(add_picked_sensor(&mut cfg, 0, &sensor));
        assert!(!add_picked_sensor(&mut cfg, 0, &sensor));
        assert!(!add_picked_sensor(&mut cfg, 2, &sensor));
        let count = cfg
            .monitor
            .rows
            .iter()
            .flat_map(|r| &r.fields)
            .filter(|f| f.as_str() == sensor.id)
            .count();
        assert_eq!(count, 1);
        assert_eq!(cfg.monitor.custom[&sensor.id], "GPU CORE");
    }

    #[test]
    fn add_field_to_row_scans_every_row() {
        let mut cfg = Config::defaults();
        assert!(add_field_to_row(&mut cfg, 0, "fan_rpm"));
        assert!(!add_field_to_row(&mut cfg, 4, "fan_rpm"));
        assert!(!add_field_to_row(&mut cfg, 99, "swap"));
    }

    #[test]
    fn remove_field_everywhere_clears_all_rows() {
        let mut cfg = Config::defaults();
        cfg.monitor.rows[0].fields.push("gpu".into());
        assert!(remove_field_everywhere(&mut cfg, "gpu"));
        assert!(cfg
            .monitor
            .rows
            .iter()
            .all(|r| !r.fields.iter().any(|f| f == "gpu")));
        assert!(!remove_field_everywhere(&mut cfg, "gpu"));
    }

    #[test]
    fn unchecking_keeps_style_and_thresholds() {
        let mut cfg = Config::defaults();
        cfg.monitor.field_style.insert("gpu".into(), Style::Bar);
        cfg.thresholds.insert(
            "gpu".into(),
            Threshold {
                warn: Some(90.0),
                crit: None,
                direction: Direction::Above,
            },
        );

        assert!(remove_field_everywhere(&mut cfg, "gpu"));
        assert_eq!(cfg.monitor.field_style.get("gpu"), Some(&Style::Bar));
        assert_eq!(cfg.thresholds["gpu"].warn, Some(90.0));
    }

    #[test]
    fn unchecking_a_custom_sensor_keeps_its_label() {
        let mut cfg = Config::defaults();
        let id = "lhm:/gpu-nvidia/0/power/0";
        cfg.monitor.custom.insert(id.into(), "GPU PWR".into());
        cfg.thresholds.insert(
            id.into(),
            Threshold {
                warn: Some(80.0),
                crit: None,
                direction: Direction::Above,
            },
        );
        cfg.monitor.rows[0].fields.push(id.into());

        assert!(remove_field_everywhere(&mut cfg, id));
        assert_eq!(cfg.monitor.custom[id], "GPU PWR");
        assert_eq!(cfg.thresholds[id].warn, Some(80.0));
    }

    #[test]
    fn forget_removes_rows_custom_style_and_threshold() {
        let mut cfg = Config::defaults();
        let id = "lhm:/gpu-nvidia/0/power/0";
        cfg.monitor.custom.insert(id.into(), "GPU PWR".into());
        cfg.monitor.field_style.insert(id.into(), Style::Bar);
        cfg.thresholds.insert(id.into(), Threshold::default());
        cfg.monitor.rows[0].fields.push(id.into());

        forget_custom_field(&mut cfg, id);

        assert!(!cfg.monitor.custom.contains_key(id));
        assert!(!cfg.monitor.field_style.contains_key(id));
        assert!(!cfg.thresholds.contains_key(id));
        assert!(cfg
            .monitor
            .rows
            .iter()
            .all(|r| !r.fields.iter().any(|f| f == id)));
    }

    #[test]
    fn picker_label_is_uppercase_and_short() {
        let mut cfg = Config::defaults();
        let sensor = PickerSensor {
            id: "lhm:/gpu/0/temperature/1".into(),
            text: "GPU Memory Junction Temperature".into(),
            unit: "°C".into(),
            value: 50.0,
        };
        assert!(add_picked_sensor(&mut cfg, 0, &sensor));
        assert_eq!(cfg.monitor.custom[&sensor.id], "GPU MEMORY J");
    }

    #[test]
    fn preview_includes_custom_sensor_values() {
        let mut cfg = Config::defaults();
        let id = "lhm:/gpu-nvidia/0/power/0".to_string();
        cfg.monitor.custom.insert(id.clone(), "GPU PACKAGE".into());
        let s = preview_sample(&cfg);
        let v = s.custom.get(&id).expect("custom sensor missing from preview");
        assert_eq!(v.unit, "W");
        assert_eq!(v.value, 42.0);
    }

    #[test]
    fn missing_only_when_the_id_is_absent() {
        let known = vec!["lhm:/gpu/0/temperature/0".to_string()];
        assert!(!is_missing(&known, "lhm:/gpu/0/temperature/0"));
        assert!(is_missing(&known, "lhm:/gpu/0/temperature/1"));
    }
}
