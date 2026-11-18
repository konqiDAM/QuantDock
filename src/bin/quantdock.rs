// No console window for the released binary.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use quantdock::monitor_config::{self, Config, Corner, Style, Watcher};
use quantdock::monitor_sample::{Sample, Value};
use quantdock::{hf, model, server};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
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
enum ProfileDialog {
    Load(usize),
    Overwrite(usize),
}

/// One open HF download dialog. Dropping it (setting `Manager::dl` to `None`) closes
/// the dialog; a running download keeps going to completion unless it is cancelled.
struct DownloadDialog {
    /// Raw repo/URL input, kept so a bad value can be corrected in place.
    input: String,
    /// Parsed `(repo, branch)` of the current listing.
    target: Option<(String, String)>,
    /// Last listing, kept when a download fails so the pick survives.
    files: Vec<hf::HfFile>,
    selected: usize,
    state: DownloadState,
}

impl DownloadDialog {
    fn new() -> Self {
        Self {
            input: String::new(),
            target: None,
            files: Vec::new(),
            selected: 0,
            state: DownloadState::Idle,
        }
    }

    fn busy(&self) -> bool {
        !matches!(self.state, DownloadState::Idle)
    }

    /// A running download owns a cancel flag and a `.part` file; a listing owns neither.
    fn download_running(&self) -> bool {
        matches!(self.state, DownloadState::Running { .. })
    }

    /// Drain the worker's queued messages and settle the state. Returns what the manager must do:
    /// show `note` (red when `error`) and optionally reload or close the dialog.
    fn drain(&mut self) -> Option<DownloadAction> {
        // Take the channel out so the state can be settled while it is drained.
        match std::mem::replace(&mut self.state, DownloadState::Idle) {
            DownloadState::Idle => None,
            DownloadState::Listing(rx) => match rx.try_recv() {
                Ok(Ok(files)) => {
                    self.files = files;
                    self.selected = 0;
                    None
                }
                Ok(Err(e)) => Some(DownloadAction::note(format!("listing failed: {e}"), true)),
                Err(TryRecvError::Empty) => {
                    self.state = DownloadState::Listing(rx);
                    None
                }
                Err(TryRecvError::Disconnected) => {
                    Some(DownloadAction::note("listing failed", true))
                }
            },
            DownloadState::Running { rx, cancel, mut bytes, mut total } => {
                let mut action = None;
                loop {
                    match rx.try_recv() {
                        Ok(hf::Progress::Downloading { bytes: b, total: t }) => {
                            bytes = b;
                            // A response may honestly report 0: keep the listing size then.
                            if t > 0 {
                                total = t;
                            }
                        }
                        Ok(hf::Progress::Done { folder, .. }) => {
                            action = Some(DownloadAction {
                                note: format!("downloaded {}", folder.display()),
                                error: false,
                                close: true,
                                reload: true,
                            });
                            break;
                        }
                        Ok(hf::Progress::Failed(e)) => {
                            action =
                                Some(DownloadAction::note(format!("download failed: {e}"), true));
                            break;
                        }
                        Ok(hf::Progress::Cancelled) => {
                            action = Some(DownloadAction::note("download cancelled", false));
                            break;
                        }
                        Ok(hf::Progress::Listing) => {}
                        Err(TryRecvError::Empty) => {
                            self.state =
                                DownloadState::Running { rx, cancel, bytes, total };
                            break;
                        }
                        Err(TryRecvError::Disconnected) => {
                            action = Some(DownloadAction::note("download failed", true));
                            break;
                        }
                    }
                }
                action
            }
        }
    }
}

/// What `Manager` does after `DownloadDialog::drain`.
struct DownloadAction {
    note: String,
    error: bool,
    /// Success closes the dialog; a failure or cancel keeps it open with the list.
    close: bool,
    reload: bool,
}

impl DownloadAction {
    fn note(text: impl Into<String>, error: bool) -> Self {
        Self { note: text.into(), error, close: false, reload: false }
    }
}

enum DownloadState {
    Idle,
    /// `list_ggufs` runs on a worker thread; the frame never waits on HTTP.
    Listing(std::sync::mpsc::Receiver<Result<Vec<hf::HfFile>, String>>),
    Running {
        rx: std::sync::mpsc::Receiver<hf::Progress>,
        cancel: Arc<AtomicBool>,
        bytes: u64,
        total: u64,
    },
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
    hardware: Vec<String>,
    hardware_id: String,
    group: String,
}

fn picker_list() -> Vec<PickerSensor> {
    let Some(items) = quantdock::monitor_lhm::fetch_tree() else {
        return Vec::new();
    };
    items
        .into_iter()
        .map(|s| PickerSensor {
            id: s.id,
            text: s.text,
            unit: s.unit,
            value: s.value,
            hardware: s.hardware,
            hardware_id: s.hardware_id,
            group: s.group,
        })
        .collect()
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

fn hardware_category(hardware_id: &str) -> &'static str {
    if hardware_id.starts_with("/intelcpu")
        || hardware_id.starts_with("/amdcpu")
        || hardware_id.starts_with("/cpu")
    {
        "CPU"
    } else if hardware_id.starts_with("/gpu-nvidia")
        || hardware_id.starts_with("/nvidiagpu")
        || hardware_id.starts_with("/atigpu")
        || hardware_id.starts_with("/gpu-amd")
        || hardware_id.starts_with("/gpu")
    {
        "GPU"
    } else if hardware_id.starts_with("/ram") || hardware_id.starts_with("/vram") {
        "Memory"
    } else if hardware_id.starts_with("/motherboard") || hardware_id.starts_with("/lpc") {
        "Board"
    } else if hardware_id.starts_with("/nvme")
        || hardware_id.starts_with("/hdd")
        || hardware_id.starts_with("/ssd")
        || hardware_id.starts_with("/storage")
    {
        "Storage"
    } else if hardware_id.starts_with("/nic") {
        "Network"
    } else {
        "Other"
    }
}

fn category_rank(name: &str) -> usize {
    ["CPU", "GPU", "Memory", "Board", "Storage", "Network", "Other"]
        .iter()
        .position(|n| *n == name)
        .unwrap_or(usize::MAX)
}

struct SensorCategory {
    name: &'static str,
    hardware: Vec<SensorHardware>,
}

struct SensorHardware {
    path: Vec<String>,
    groups: Vec<SensorGroupView>,
}

struct SensorGroupView {
    name: String,
    sensors: Vec<usize>,
}

fn sensor_matches(sensor: &PickerSensor, needle: &str) -> bool {
    needle.is_empty()
        || sensor.text.to_lowercase().contains(needle)
        || sensor.id.to_lowercase().contains(needle)
        || sensor.group.to_lowercase().contains(needle)
        || sensor
            .hardware
            .iter()
            .any(|h| h.to_lowercase().contains(needle))
}

fn build_sensor_tree(sensors: &[PickerSensor], needle: &str) -> Vec<SensorCategory> {
    let needle = needle.trim().to_lowercase();
    let mut categories: Vec<SensorCategory> = Vec::new();

    for (index, sensor) in sensors.iter().enumerate() {
        if !sensor_matches(sensor, &needle) {
            continue;
        }
        let category = hardware_category(&sensor.hardware_id);
        let category_index = match categories.iter().position(|c| c.name == category) {
            Some(i) => i,
            None => {
                categories.push(SensorCategory {
                    name: category,
                    hardware: Vec::new(),
                });
                categories.len() - 1
            }
        };
        let path = if sensor.hardware.is_empty() {
            vec!["Other".to_owned()]
        } else {
            sensor.hardware.clone()
        };
        let category_ref = &mut categories[category_index];
        let hardware_index = match category_ref.hardware.iter().position(|h| h.path == path) {
            Some(i) => i,
            None => {
                category_ref.hardware.push(SensorHardware {
                    path,
                    groups: Vec::new(),
                });
                category_ref.hardware.len() - 1
            }
        };
        let group_name = if sensor.group.is_empty() {
            "Other"
        } else {
            sensor.group.as_str()
        };
        let hardware_ref = &mut category_ref.hardware[hardware_index];
        let group_index = match hardware_ref.groups.iter().position(|g| g.name == group_name) {
            Some(i) => i,
            None => {
                hardware_ref.groups.push(SensorGroupView {
                    name: group_name.to_owned(),
                    sensors: Vec::new(),
                });
                hardware_ref.groups.len() - 1
            }
        };
        hardware_ref.groups[group_index].sensors.push(index);
    }

    categories.sort_by_key(|category| category_rank(category.name));
    categories
}

fn sensor_tree_ui(
    ui: &mut egui::Ui,
    cfg: &Config,
    sensors: &[PickerSensor],
    needle: &str,
    rows_empty: bool,
    toggle: &mut Option<(String, bool)>,
) {
    let tree = build_sensor_tree(sensors, needle);
    if tree.is_empty() {
        if !needle.is_empty() {
            ui.weak("No sensor matches the search.");
        }
        return;
    }

    for category in &tree {
        let open = !needle.is_empty() || matches!(category.name, "CPU" | "GPU");
        egui::CollapsingHeader::new(category.name)
            .id_salt(("sensor_category", category.name))
            .default_open(open)
            .open(if needle.is_empty() { None } else { Some(true) })
            .show(ui, |ui| {
                for hardware in &category.hardware {
                    render_sensor_hardware(
                        ui,
                        cfg,
                        sensors,
                        category.name,
                        &hardware.path,
                        hardware,
                        needle,
                        rows_empty,
                        toggle,
                    );
                }
            });
    }
}

fn render_sensor_hardware(
    ui: &mut egui::Ui,
    cfg: &Config,
    sensors: &[PickerSensor],
    category: &str,
    path: &[String],
    hardware: &SensorHardware,
    needle: &str,
    rows_empty: bool,
    toggle: &mut Option<(String, bool)>,
) {
    let Some((first, rest)) = path.split_first() else {
        return;
    };
    let open = !needle.is_empty() || matches!(category, "CPU" | "GPU");
    egui::CollapsingHeader::new(first.as_str())
        .id_salt(("sensor_hardware", category, path))
        .default_open(open)
        .open(if needle.is_empty() { None } else { Some(true) })
        .show(ui, |ui| {
            if rest.is_empty() {
                for group in &hardware.groups {
                    let open_group = !needle.is_empty()
                        || (matches!(category, "CPU" | "GPU") && group.name == "Temperatures");
                    egui::CollapsingHeader::new(format!("{} ({})", group.name, group.sensors.len()))
                        .id_salt(("sensor_group", category, path, group.name.as_str()))
                        .default_open(open_group)
                        .open(if needle.is_empty() { None } else { Some(true) })
                        .show(ui, |ui| {
                            for &index in &group.sensors {
                                let sensor = &sensors[index];
                                let mut on = field_checked(cfg, &sensor.id);
                                let can = on || !rows_empty;
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_enabled(can, egui::Checkbox::new(&mut on, &sensor.text))
                                        .on_hover_text(&sensor.id)
                                        .changed()
                                    {
                                        *toggle = Some((sensor.id.clone(), on));
                                    }
                                    ui.weak(format!("{} {}", sensor.value, sensor.unit));
                                });
                            }
                        });
                }
            } else {
                render_sensor_hardware(
                    ui, cfg, sensors, category, rest, hardware, needle, rows_empty, toggle,
                );
            }
        });
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


/// Startup hide passes: eframe re-shows the root window after its first painted frame,
/// so a single `Visible(false)` would lose.
const HIDE_PASSES: u32 = 3;

/// One bootstrap pass: `(send Visible(false), hide_pending, hidden)`. `reveal` zeroes
/// `hide_pending`, which stops the bootstrap without a send.
fn hide_tick(pending: bool, passes: u32) -> (bool, bool, bool) {
    if !pending {
        return (false, false, false);
    }
    let done = passes + 1 >= HIDE_PASSES;
    (true, !done, done)
}

/// The reveal core behind `show_window`: show, un-minimize and focus, then cancel any
/// remaining bootstrap pass so a later tick cannot hide the window again.
fn reveal(hidden: &mut bool, hide_pending: &mut bool, hide_passes: &mut u32, ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    *hidden = false;
    *hide_pending = false;
    *hide_passes = 0;
}

/// Tray menu item ids, read by both the menu builder and its test.
const TRAY_SHOW: &str = "show";
const TRAY_HIDE: &str = "hide";
const TRAY_MONITOR: &str = "monitor";
const TRAY_EXIT: &str = "exit";

/// Start/Close label for the overlay entry. The menu builder and its live rewrite share it.
fn monitor_label(monitor_up: bool) -> &'static str {
    if monitor_up {
        "Close monitor"
    } else {
        "Start monitor"
    }
}

/// The tray menu as `(id, label)`. Only the overlay label depends on state.
fn tray_menu_items(monitor_up: bool) -> [(&'static str, &'static str); 4] {
    [
        (TRAY_SHOW, "Show QuantDock"),
        (TRAY_HIDE, "Hide QuantDock"),
        (TRAY_MONITOR, monitor_label(monitor_up)),
        (TRAY_EXIT, "Exit"),
    ]
}

/// Tray tooltip: the two live parts plus the app name. `serving` is (alias, name), the
/// same pair the header shows; `None` means the server is down, as the header shows.
fn tooltip_text(serving: Option<(&str, &str)>, monitor_up: bool) -> String {
    let server = match serving {
        Some((alias, name)) => format!("serving {alias} — {name}"),
        None => "server down".to_owned(),
    };
    let monitor = if monitor_up { "up" } else { "down" };
    format!("QuantDock — {server} · monitor {monitor}")
}

/// Manual-reset "show the window" event: the wake thread checks it, `logic()` consumes it.
/// `HANDLE` is neither `Send` nor `Sync`, so the static holds its raw value.
static SHOW_EVENT_HANDLE: OnceLock<isize> = OnceLock::new();

/// Staged quit. `logic()` advances and reveals; the unsaved-settings question is painted
/// by `ui()` from the stage, because `logic()` may not run UI code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitStage {
    /// Waiting for the answer to "Exit without saving settings?".
    Confirm,
    /// A running download was signalled to stop; waiting for its worker.
    CancelDownload,
    /// The taskbar-less overlay goes before the manager, so nothing is orphaned.
    CloseMonitor,
    /// Tray handles dropped, viewport closing.
    Quit,
}

/// How long a cancelling download may take before the quit goes on without it.
const EXIT_DEADLINE_MS: u64 = 5000;

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
    /// App-owned visibility: `ViewportInfo::visible()` is unusable on Windows.
    hidden: bool,
    /// A startup hide still owes its bootstrap passes.
    hide_pending: bool,
    /// Passes sent since `hide_pending` was set.
    hide_passes: u32,
    /// Exit in progress; `None` is idle.
    exit_stage: Option<ExitStage>,
    /// Set once the download cancel was sent, so the wait has a deadline.
    exit_cancel_at: Option<Instant>,
    /// Set on the quit stage: the close handler then stops cancelling.
    quitting: bool,
    /// Kept alive for the process: dropping the handle removes the icon.
    _tray: Option<tray_icon::TrayIcon>,
    /// The overlay entry, rewritten in place when `monitor_up` changes.
    tray_monitor: Option<tray_icon::menu::MenuItem>,
    /// False when the tray failed to build; the window then never hides itself.
    tray_ok: bool,
    tray_tx: std::sync::mpsc::Sender<String>,
    tray_rx: std::sync::mpsc::Receiver<String>,
    /// The wake thread needs a context, so it is spawned by the first `logic()` pass.
    wake_started: bool,
    profile_slot: usize,
    profile_dialog: Option<ProfileDialog>,
    dl: Option<DownloadDialog>,
    disk_changed: bool,
    custom_id_input: String,
    monitor_up: bool,
    /// `monitor_up` last pushed to the tray label.
    tray_label_up: bool,
    /// Last tooltip written to the tray, so an unchanged text is not re-sent.
    tooltip: String,
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
    fn new(start_hidden: bool) -> Self {
        let root = model::models_root();
        // Startup-only, before any download can own a `.part` folder.
        let swept = hf::sweep_partials(&root);
        let (load_tx, load_rx) = std::sync::mpsc::channel();
        let (sensors_tx, sensors_rx) = std::sync::mpsc::channel();
        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        let (picker_tx, picker_rx) = std::sync::mpsc::channel();
        let (tray_tx, tray_rx) = std::sync::mpsc::channel();
        let (tray, tray_monitor) = match make_tray() {
            Some((tray, monitor)) => (Some(tray), Some(monitor)),
            None => (None, None),
        };
        let tray_ok = tray.is_some();
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
            // Never seed `hidden`: only a sent hide command may claim the window is out.
            hidden: false,
            hide_pending: start_hidden,
            hide_passes: 0,
            exit_stage: None,
            exit_cancel_at: None,
            quitting: false,
            _tray: tray,
            tray_monitor,
            tray_ok,
            tray_tx,
            tray_rx,
            wake_started: false,
            profile_slot: 0,
            profile_dialog: None,
            dl: None,
            disk_changed: false,
            custom_id_input: String::new(),
            monitor_up: false,
            tray_label_up: false,
            tooltip: String::new(),
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
        if !swept.is_empty() {
            m.note(
                format!("removed {} abandoned partial download folder(s)", swept.len()),
                false,
            );
        }
        if !m.tray_ok {
            // No tray means no way back: refuse to hide.
            m.hide_pending = false;
            m.note(
                "tray icon unavailable — hiding to tray is disabled",
                start_hidden,
            );
        }
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

    /// (alias, name) of the served model, with the header's "unknown" fallback. Shared by
    /// the header and the tray tooltip so the two cannot disagree.
    fn serving_id(&self) -> Option<(String, String)> {
        let st = self.status.as_ref()?;
        Some(match self.loaded_index() {
            Some(i) => (
                self.models[i].cfg.alias.clone(),
                self.models[i].cfg.name.clone(),
            ),
            None => (st.served_id.clone(), "unknown".to_owned()),
        })
    }

    /// Index of the model the server is actually serving.
    fn loaded_index(&self) -> Option<usize> {
        let st = self.status.as_ref()?;
        let m = model::loaded(&self.models, st)?;
        self.models.iter().position(|x| x.toml_path == m.toml_path)
    }

    /// The one reveal path: tray Show, the second-instance event and the Exit confirm.
    fn show_window(&mut self, ctx: &egui::Context) {
        reveal(
            &mut self.hidden,
            &mut self.hide_pending,
            &mut self.hide_passes,
            ctx,
        );
    }

    /// Tray Hide. Zeroing `hide_passes` keeps the counter honest for a later bootstrap.
    fn hide_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        self.hidden = true;
        self.hide_pending = false;
        self.hide_passes = 0;
    }

    /// Spawned from `logic()`, not `new()`: only there is a context to wake.
    fn start_wake(&mut self, ctx: &egui::Context) {
        if self.wake_started {
            return;
        }
        self.wake_started = true;
        let (ctx, tx) = (ctx.clone(), self.tray_tx.clone());
        std::thread::spawn(move || wake_loop(ctx, tx));
    }

    /// One fresh mutex check decides the branch, so a stale label (button or tray) can
    /// never start a second overlay. `monitor_up` is label feedback, not the branch.
    fn toggle_monitor(&mut self) {
        if Self::monitor_running() {
            Self::close_monitor();
            self.monitor_up = false;
            self.note("monitor closed", false);
        } else {
            match Self::spawn_monitor() {
                Ok(()) => {
                    // The spawn is async; waiting for `poll()` would lag the label ~1.5 s.
                    self.monitor_up = true;
                    self.note("monitor started", false);
                }
                Err(e) => self.note(format!("monitor failed: {e}"), true),
            }
        }
    }

    /// Tray label and tooltip, rewritten only when their text would change.
    fn sync_tray(&mut self) {
        if self.tray_label_up != self.monitor_up {
            self.tray_label_up = self.monitor_up;
            if let Some(item) = &self.tray_monitor {
                item.set_text(monitor_label(self.monitor_up));
            }
        }
        let serving = self.serving_id();
        let text = tooltip_text(
            serving.as_ref().map(|(alias, name)| (alias.as_str(), name.as_str())),
            self.monitor_up,
        );
        if text != self.tooltip {
            if let Some(tray) = &self._tray {
                // Only cache a delivered tooltip, so a refused write retries next pass.
                if tray.set_tooltip(Some(&text)).is_ok() {
                    self.tooltip = text;
                }
            }
        }
    }

    /// Tray menu ids: Show/Hide and the overlay toggle act now, Exit starts the quit.
    fn drain_tray(&mut self, ctx: &egui::Context) {
        while let Ok(id) = self.tray_rx.try_recv() {
            match id.as_str() {
                TRAY_SHOW => self.show_window(ctx),
                TRAY_HIDE => self.hide_window(ctx),
                TRAY_MONITOR => self.toggle_monitor(),
                TRAY_EXIT => self.start_exit(ctx),
                _ => {}
            }
        }
    }

    /// Tray Exit. A dirty Settings draft asks first; otherwise the quit starts at once.
    fn start_exit(&mut self, ctx: &egui::Context) {
        if self.exit_stage.is_some() || self.quitting {
            // A quit already in flight: a second Exit while the question is up only brings it back.
            if self.exit_stage == Some(ExitStage::Confirm) {
                self.show_window(ctx);
            }
            return;
        }
        // A deadline left by an earlier quit must not make the next wait look spent.
        self.exit_cancel_at = None;
        self.exit_stage = Some(if self.dirty {
            // The question is a window: it needs a visible manager to be answered in.
            self.show_window(ctx);
            ExitStage::Confirm
        } else {
            ExitStage::CancelDownload
        });
    }

    /// One pass of the quit machine. Never blocks: a cancelling download is polled.
    fn advance_exit(&mut self, ctx: &egui::Context) {
        loop {
            match self.exit_stage {
                // Idle, or waiting for the answer `ui()` collects.
                None | Some(ExitStage::Confirm) => return,
                Some(ExitStage::CancelDownload) => {
                    let running = self
                        .dl
                        .as_ref()
                        .is_some_and(DownloadDialog::download_running);
                    if running {
                        ctx.request_repaint_after(Duration::from_millis(100));
                        let Some(at) = self.exit_cancel_at else {
                            if let Some(dl) = self.dl.as_ref() {
                                if let DownloadState::Running { cancel, .. } = &dl.state {
                                    cancel.store(true, Ordering::Relaxed);
                                }
                            }
                            self.exit_cancel_at = Some(Instant::now());
                            return;
                        };
                        // The worker deletes its own `.part` once it sees the flag.
                        if at.elapsed() < Duration::from_millis(EXIT_DEADLINE_MS) {
                            return;
                        }
                        // An unresponsive worker is abandoned: the startup sweep takes
                        // its `.part` on the next launch.
                    }
                    self.exit_stage = Some(ExitStage::CloseMonitor);
                }
                Some(ExitStage::CloseMonitor) => {
                    if Self::monitor_running() {
                        Self::close_monitor();
                    }
                    self.exit_stage = Some(ExitStage::Quit);
                }
                Some(ExitStage::Quit) => {
                    self.quitting = true;
                    self.exit_stage = None;
                    // Both handles: the icon goes now, and the menu item cannot outlive
                    // the tray that owns it, so the item drops first.
                    self.tray_monitor = None;
                    self._tray = None;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
            }
        }
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

/// Distance from `(x, y)` to the segment `a`-`b`.
fn seg_near(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let (apx, apy) = (x - a.0, y - a.1);
    let t = ((apx * abx + apy * aby) / (abx * abx + aby * aby)).clamp(0.0, 1.0);
    ((apx - abx * t).powi(2) + (apy - aby * t).powi(2)).sqrt()
}

/// A rounded square with a "Q", drawn at runtime so the app needs no asset file.
fn tray_icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let c = (s - 1.0) / 2.0;
    let half = c - s * 0.04;
    let corner = s * 0.22;
    let ring = s * 0.27;
    let stroke = s * 0.10;
    let tail = (
        (c + ring * 0.55, c + ring * 0.55),
        (c + ring * 1.05, c + ring * 1.05),
    );
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32, y as f32);
            let dx = ((fx - c).abs() - (half - corner)).max(0.0);
            let dy = ((fy - c).abs() - (half - corner)).max(0.0);
            let inside = dx * dx + dy * dy <= corner * corner;
            let d = ((fx - c).powi(2) + (fy - c).powi(2)).sqrt() - ring;
            let letter = d.abs() <= stroke / 2.0
                || seg_near(fx, fy, tail.0, tail.1) <= stroke / 2.0;
            let rgb = if !inside {
                [0, 0, 0]
            } else if letter {
                [235, 240, 235]
            } else {
                [46, 120, 74]
            };
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], if inside { 255 } else { 0 }]);
        }
    }
    px
}

/// Builds the tray on the calling thread: its hidden window is pumped by that thread's
/// event loop, so this must run in `Manager::new`. The monitor item is returned because
/// `TrayIcon` has no menu getter and the label must be rewritten on state changes.
fn make_tray() -> Option<(tray_icon::TrayIcon, tray_icon::menu::MenuItem)> {
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
    let icon = tray_icon::Icon::from_rgba(tray_icon_rgba(32), 32, 32).ok()?;
    let menu = Menu::new();
    let items = tray_menu_items(false);
    let (exit, rest) = items.split_last()?;
    // A partial menu is no tray: fail here so the caller's failure policy fires.
    let mut monitor = None;
    for (id, label) in rest {
        let item = MenuItem::with_id(*id, *label, true, None);
        menu.append(&item).ok()?;
        if *id == TRAY_MONITOR {
            monitor = Some(item);
        }
    }
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&MenuItem::with_id(exit.0, exit.1, true, None))
        .ok()?;
    let tray = tray_icon::TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("QuantDock")
        .with_icon(icon)
        .build()
        .ok()?;
    Some((tray, monitor?))
}

/// Tray wake thread. The 100 ms poll is cheap; a repaint is requested only when there is
/// something for `logic()` to act on, so the idle 2 s cadence is not turned into 10 Hz.
fn wake_loop(ctx: egui::Context, tx: std::sync::mpsc::Sender<String>) {
    let events = tray_icon::menu::MenuEvent::receiver();
    loop {
        let mut wake = false;
        if let Ok(ev) = events.recv_timeout(Duration::from_millis(100)) {
            if tx.send(ev.id.0).is_err() {
                return;
            }
            wake = true;
        }
        // The second-instance signal is checked here and consumed by `logic()`.
        if show_signalled() {
            wake = true;
        }
        if wake {
            ctx.request_repaint();
        }
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

/// One-decimal size for the progress text, without a trailing `.0`.
fn size_text(bytes: u64) -> String {
    let (div, unit) = if bytes >= 1_000_000_000 {
        (1_000_000_000.0, "GB")
    } else {
        (1_000_000.0, "MB")
    };
    let n = format!("{:.1}", bytes as f64 / div);
    format!("{} {unit}", n.trim_end_matches(".0"))
}

/// `12.3 MB / 40 GB · 31%`. An unknown total is indeterminate: bytes only, no percent.
fn progress_label(bytes: u64, total: u64) -> String {
    if total == 0 {
        return size_text(bytes);
    }
    let percent = bytes.min(total).saturating_mul(100) / total;
    format!("{} / {} · {percent}%", size_text(bytes), size_text(total))
}

/// Left margin that centers a `cap`-wide settings column in `avail` px.
fn settings_left_offset(avail: f32, cap: f32) -> f32 {
    ((avail - cap) / 2.0).max(0.0)
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

    fn profiles_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        for row in 0..2 {
            ui.horizontal(|ui| {
                for col in 0..5 {
                    let slot = row * 5 + col;
                    let filled = self.cfg.profile(slot).is_some();
                    let text = if filled {
                        egui::RichText::new((slot + 1).to_string())
                            .color(self.cfg.theme.accent)
                    } else {
                        egui::RichText::new("–")
                    };
                    let hover = if filled { "Saved" } else { "Empty" };
                    if ui
                        .selectable_label(self.profile_slot == slot, text)
                        .on_hover_text(hover)
                        .clicked()
                    {
                        self.profile_slot = slot;
                    }
                }
            });
        }

        let slot = self.profile_slot;
        let filled = self.cfg.profile(slot).is_some();
        ui.weak(format!(
            "Selected: Slot {} — {}",
            slot + 1,
            if filled { "saved" } else { "empty" }
        ));
        ui.horizontal(|ui| {
            if ui
                .button(format!("Save current to slot {}", slot + 1))
                .clicked()
            {
                if filled {
                    self.profile_dialog = Some(ProfileDialog::Overwrite(slot));
                } else {
                    self.cfg.set_profile(slot);
                    changed = true;
                }
            }
            if ui
                .add_enabled(
                    filled,
                    egui::Button::new(format!("Load slot {}", slot + 1)),
                )
                .clicked()
            {
                if self.cfg.matches_a_profile() {
                    if let Some(profile) = self.cfg.profile(slot).cloned() {
                        self.cfg.apply_profile(&profile);
                        changed = true;
                    }
                } else {
                    self.profile_dialog = Some(ProfileDialog::Load(slot));
                }
            }
        });
        changed
    }

    fn profile_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.profile_dialog else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Profiles")
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| match dialog {
                ProfileDialog::Load(slot) => {
                    ui.label(
                        egui::RichText::new(format!("Load profile {}?", slot + 1)).strong(),
                    );
                    ui.label("The current layout is not saved to any profile.");
                    ui.label("This also discards unsaved overlay edits in the draft.");
                    ui.horizontal(|ui| {
                        if ui.button("Load anyway").clicked() {
                            confirm = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                }
                ProfileDialog::Overwrite(slot) => {
                    ui.label(
                        egui::RichText::new(format!("Overwrite slot {}?", slot + 1)).strong(),
                    );
                    ui.label("The saved profile in this slot is replaced by the current layout.");
                    ui.horizontal(|ui| {
                        if ui.button("Overwrite").clicked() {
                            confirm = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                }
            });

        if confirm {
            match dialog {
                ProfileDialog::Load(slot) => {
                    if let Some(profile) = self.cfg.profile(slot).cloned() {
                        self.cfg.apply_profile(&profile);
                        self.dirty = true;
                    }
                }
                ProfileDialog::Overwrite(slot) => {
                    self.cfg.set_profile(slot);
                    self.dirty = true;
                }
            }
            self.profile_dialog = None;
        } else if cancel {
            self.profile_dialog = None;
        }
    }

    /// Retire a finished listing or download. Runs from `logic()`, never from `ui()`,
    /// so the worker is drained even while the window is hidden.
    fn drain_download(&mut self) {
        let Some(action) = self.dl.as_mut().and_then(DownloadDialog::drain) else {
            return;
        };
        self.note(action.note, action.error);
        if action.reload {
            self.reload();
        }
        if action.close {
            self.dl = None;
        }
    }

    fn download_dialog_ui(&mut self, ctx: &egui::Context) {
        if self.dl.is_none() {
            return;
        }
        let (mut fetch, mut start, mut cancel, mut close) = (false, false, false, false);
        if let Some(dl) = self.dl.as_mut() {
            egui::Window::new("Download model")
                .collapsible(false)
                .resizable(false)
                .default_width(460.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Repo or URL");
                        let edit = ui.add_enabled(
                            !dl.busy(),
                            egui::TextEdit::singleline(&mut dl.input)
                                .hint_text("owner/name or huggingface.co link")
                                .desired_width(300.0),
                        );
                        if edit.changed() {
                            // The listed files belong to the repo being edited away from.
                            dl.files.clear();
                            dl.selected = 0;
                            dl.target = None;
                        }
                    });
                    if !dl.files.is_empty() {
                        ui.separator();
                        egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                            for (i, f) in dl.files.iter().enumerate() {
                                // Nested repo paths share a basename, so show the path when it has one.
                                let name = if f.path.contains(['/', '\\']) {
                                    f.path.clone()
                                } else {
                                    Path::new(&f.path)
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| f.path.clone())
                                };
                                let label = format!("{name} ({})", fmt_size(f.size));
                                if ui
                                    .add_enabled(
                                        !dl.busy(),
                                        egui::RadioButton::new(dl.selected == i, label),
                                    )
                                    .clicked()
                                {
                                    dl.selected = i;
                                }
                            }
                        });
                    }
                    ui.separator();
                    match &dl.state {
                        DownloadState::Idle => {
                            ui.horizontal(|ui| {
                                fetch = ui
                                    .add_enabled(
                                        !dl.input.trim().is_empty(),
                                        egui::Button::new("Fetch files"),
                                    )
                                    .clicked();
                                start = ui
                                    .add_enabled(
                                        !dl.files.is_empty(),
                                        egui::Button::new("Download"),
                                    )
                                    .clicked();
                                close = ui.button("Close").clicked();
                            });
                        }
                        DownloadState::Listing(_) => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("listing .gguf files…");
                                // Closing drops the receiver; the worker's send is ignored.
                                close = ui.button("Close").clicked();
                            });
                        }
                        DownloadState::Running { bytes, total, .. } => {
                            let fraction = if *total == 0 {
                                0.0
                            } else {
                                *bytes as f32 / *total as f32
                            };
                            ui.add(
                                egui::ProgressBar::new(fraction)
                                    .animate(*total == 0)
                                    .text(progress_label(*bytes, *total)),
                            );
                            cancel = ui.button("Cancel").clicked();
                        }
                    }
                });
        }

        if fetch {
            match self.dl.as_ref().and_then(|dl| hf::parse_repo(&dl.input)) {
                Some((repo, branch)) => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let (r, b) = (repo.clone(), branch.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(hf::list_ggufs(&r, &b));
                    });
                    if let Some(dl) = self.dl.as_mut() {
                        dl.target = Some((repo, branch));
                        dl.files.clear();
                        dl.selected = 0;
                        dl.state = DownloadState::Listing(rx);
                    }
                }
                None => self.note("enter owner/name or a huggingface.co URL", true),
            }
        }
        if start {
            let spec = self.dl.as_ref().and_then(|dl| {
                let (repo, branch) = dl.target.clone()?;
                let f = dl.files.get(dl.selected)?;
                Some(hf::DownloadSpec {
                    repo,
                    branch,
                    path: f.path.clone(),
                    size: f.size,
                })
            });
            match spec {
                Some(spec) => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let cancel_flag = Arc::new(AtomicBool::new(false));
                    let size = spec.size;
                    hf::start_download(spec, &self.root, cancel_flag.clone(), tx);
                    if let Some(dl) = self.dl.as_mut() {
                        dl.state = DownloadState::Running {
                            rx,
                            cancel: cancel_flag,
                            bytes: 0,
                            total: size,
                        };
                    }
                }
                None => self.note("fetch and pick a file first", true),
            }
        }
        if cancel {
            if let Some(DownloadState::Running { cancel, .. }) =
                self.dl.as_ref().map(|dl| &dl.state)
            {
                cancel.store(true, Ordering::Relaxed);
            }
        }
        if close {
            self.dl = None;
        }
    }

    /// The unsaved-settings question. Painted from `exit_stage`, never from `logic()`.
    fn exit_dialog_ui(&mut self, ctx: &egui::Context) {
        if self.exit_stage != Some(ExitStage::Confirm) {
            return;
        }
        let (mut confirm, mut cancel) = (false, false);
        egui::Window::new("Exit QuantDock")
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Exit without saving settings?").strong());
                // Settings stays live under the dialog, so the draft can be saved while
                // this is open; the warning would then be a lie.
                if self.dirty {
                    ui.label("The Settings draft has changes that are not saved.");
                }
                ui.horizontal(|ui| {
                    if ui.button("Exit").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

        if confirm {
            self.exit_stage = Some(ExitStage::CancelDownload);
            // The answer is the only thing that moves the quit on; ask for the next pass.
            ctx.request_repaint();
        } else if cancel {
            self.exit_stage = None;
            self.exit_cancel_at = None;
        }
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
                self.picker_filter.clear();
                self.custom_id_input.clear();
                self.profile_dialog = None;
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
                ui.heading("Manager");
                changed |= ui
                    .checkbox(&mut self.cfg.start_hidden_to_tray, "Start hidden to tray")
                    .changed();

                ui.separator();
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
                ui.heading("Profiles");
                ui.weak("Save the whole overlay setup to a slot, or load one. Sensor settings stay as they are.");
                changed |= self.profiles_ui(ui);

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
                        } else if !self.picker_sensors.is_empty() {
                            sensor_tree_ui(
                                ui,
                                &self.cfg,
                                &self.picker_sensors,
                                &needle,
                                self.cfg.monitor.rows.is_empty(),
                                &mut toggle,
                            );
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
                            // Without a live list every stored id looks absent; only
                            // LHM can say one is actually missing.
                            let live = self.lhm_state == LhmState::Connected;
                            ui.separator();
                            ui.label(
                                egui::RichText::new(if live {
                                    "Stored / missing"
                                } else {
                                    "Stored"
                                })
                                .strong(),
                            );
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
                                    if live {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(230, 190, 80),
                                            "(missing)",
                                        );
                                    }
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
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.start_wake(ctx);
        // X hides while the tray can bring the window back; without a tray it is the real
        // close, so a failed tray never leaves an unreachable manager running.
        if self.tray_ok && !self.quitting && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            // X with the exit question up means "do not exit": cancel it, don't hide it
            // still pending.
            if self.exit_stage == Some(ExitStage::Confirm) {
                self.exit_stage = None;
                self.exit_cancel_at = None;
            }
            self.hide_window(ctx);
        }
        self.poll(ctx);
        self.drain_download();
        if self.dl.as_ref().is_some_and(DownloadDialog::busy) {
            // A listing or download moves at ~10 fps instead of the idle 2 s cadence.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        // A second launch only signals; `logic()` is the only consumer of the event.
        if show_signalled() {
            reset_show_event();
            self.show_window(ctx);
        }
        self.drain_tray(ctx);
        self.sync_tray();
        self.advance_exit(ctx);
        // Never fight the Quit close with a hide from a still-pending bootstrap.
        if self.hide_pending && !self.quitting {
            // eframe shows the root window again after its first painted frame, so keep
            // hiding past it; the app-owned flag takes over once the passes are spent.
            let (send, pending, now_hidden) = hide_tick(self.hide_pending, self.hide_passes);
            self.hide_passes += 1;
            self.hide_pending = pending;
            self.hidden |= now_hidden;
            if send {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let loaded = self.loaded_index();

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("QuantDock").strong().size(16.0));
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Models, "Models");
                ui.selectable_value(&mut self.tab, Tab::Settings, if self.dirty { "Settings *" } else { "Settings" });
                ui.separator();

                match self.serving_id() {
                    Some((alias, name)) => {
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
                        self.toggle_monitor();
                    }
                    if ui.button("Refresh").clicked() {
                        self.reload();
                    }
                    if self.tab == Tab::Models
                        && ui
                            .add_enabled(self.dl.is_none(), egui::Button::new("Download"))
                            .clicked()
                    {
                        self.dl = Some(DownloadDialog::new());
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
                // A 640 pt column reads well at 150% scaling (clamped to the available
                // width on narrow windows). The inner ui.vertical that used to hold it
                // gave the ScrollArea one row of height, so the column is sized
                // explicitly instead.
                let cap = 640.0_f32.min(ui.available_width());
                let offset = settings_left_offset(ui.available_width(), cap);
                let avail_h = ui.available_height();
                ui.horizontal(|ui| {
                    ui.add_space(offset);
                    ui.allocate_ui_with_layout(
                        egui::vec2(cap, avail_h),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| self.settings_ui(ui),
                    );
                });
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
        self.profile_dialog_ui(&ctx);
        self.download_dialog_ui(&ctx);
        self.exit_dialog_ui(&ctx);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LaunchAction {
    Run,
    SignalAndExit,
}

/// The mutex is created by both instances; only the loser's `GetLastError` says so.
fn decide_second_launch(last_error: windows::Win32::Foundation::WIN32_ERROR) -> LaunchAction {
    use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
    if last_error == ERROR_ALREADY_EXISTS {
        LaunchAction::SignalAndExit
    } else {
        LaunchAction::Run
    }
}

/// True when the show event is signalled. Checks only; `logic()` does the reset.
fn show_signalled() -> bool {
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::WaitForSingleObject;
    SHOW_EVENT_HANDLE.get().is_some_and(|h| unsafe {
        WaitForSingleObject(HANDLE(*h as *mut std::ffi::c_void), 0) == WAIT_OBJECT_0
    })
}

fn reset_show_event() {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::ResetEvent;
    if let Some(h) = SHOW_EVENT_HANDLE.get() {
        unsafe {
            let _ = ResetEvent(HANDLE(*h as *mut std::ffi::c_void));
        }
    }
}

/// Creates the single-instance mutex and show event. `Ok(false)` means another manager
/// owns the mutex: it was signalled to show and this process must exit without a UI
/// loop. `Err` means the guard itself could not be built: fail closed rather than risk
/// two managers racing the port and `settings.toml`.
fn single_instance() -> Result<bool, String> {
    use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, GetLastError, SetLastError};
    use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent};

    // The event first, so a launch racing the first instance has something to signal.
    let event =
        unsafe { CreateEventW(None, true, false, windows::core::w!("Local\\QuantDockManagerShow")) }
            .map_err(|e| format!("cannot create the show event: {e}"))?;
    // A stale last error would read as "another manager is running", so clear it first.
    unsafe { SetLastError(ERROR_SUCCESS) };
    let mutex = unsafe { CreateMutexW(None, true, windows::core::w!("Local\\QuantDockManager")) };
    // Both instances get a handle; only the loser's last error says it lost.
    let last_error = unsafe { GetLastError() };
    let mutex = match (mutex, decide_second_launch(last_error)) {
        // Held for the process lifetime: nobody closes it, so the mutex stays owned.
        (Ok(h), LaunchAction::Run) => h,
        (Ok(h), LaunchAction::SignalAndExit) => {
            let _ = unsafe { SetEvent(event) };
            let _ = unsafe { CloseHandle(event) };
            let _ = unsafe { CloseHandle(h) };
            return Ok(false);
        }
        (Err(e), _) => {
            let _ = unsafe { CloseHandle(event) };
            return Err(format!("cannot create the single-instance mutex: {e}"));
        }
    };
    // HANDLE has no Drop, so this opens nothing to close: the OS releases the mutex
    // when the process exits.
    let _ = mutex;
    let _ = SHOW_EVENT_HANDLE.set(event.0 as isize);
    Ok(true)
}

fn main() -> eframe::Result {
    quantdock::install_panic_log();
    match single_instance() {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(e) => {
            // A GUI-subsystem process has nowhere visible for this: refuse to start.
            eprintln!("QuantDock: {e}");
            std::process::exit(1);
        }
    }
    // Read once, never written: the settings draft owns the setting after launch.
    let start_hidden = Config::load_existing(&monitor_config::settings_path())
        .ok()
        .flatten()
        .is_some_and(|c| c.start_hidden_to_tray);
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
        Box::new(|_cc| Ok(Box::new(Manager::new(start_hidden)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use quantdock::monitor_config::{Direction, Row, Style, Threshold};

    #[test]
    fn second_launch_sets_the_event_and_exits() {
        use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_SUCCESS};
        // The loser of the mutex race signals the live instance and exits; the winner runs.
        assert_eq!(
            decide_second_launch(ERROR_ALREADY_EXISTS),
            LaunchAction::SignalAndExit
        );
        assert_eq!(decide_second_launch(ERROR_SUCCESS), LaunchAction::Run);
    }

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

    #[test]
    fn settings_column_geometry() {
        assert_eq!(settings_left_offset(1000.0, 640.0), 180.0);
        assert_eq!(settings_left_offset(640.0, 640.0), 0.0);
        assert_eq!(settings_left_offset(400.0, 640.0), 0.0);
    }

    #[test]
    fn dialog_progress_derives_percent_and_text() {
        assert_eq!(
            progress_label(12_300_000, 40_000_000_000),
            "12.3 MB / 40 GB · 0%"
        );
        assert_eq!(
            progress_label(20_000_000_000, 40_000_000_000),
            "20 GB / 40 GB · 50%"
        );
        assert_eq!(
            progress_label(30_000_000, 40_000_000),
            "30 MB / 40 MB · 75%"
        );
        // A legitimate total of 0 is indeterminate: bytes only, no percent.
        assert_eq!(progress_label(12_300_000, 0), "12.3 MB");
        assert_eq!(progress_label(0, 0), "0 MB");
    }

    fn running(rx: std::sync::mpsc::Receiver<hf::Progress>) -> DownloadDialog {
        let mut dl = DownloadDialog::new();
        dl.files = vec![hf::HfFile {
            path: "Q4_K_M/model.gguf".into(),
            size: 4_000_000_000,
        }];
        dl.state = DownloadState::Running {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            bytes: 1,
            total: 4_000_000_000,
        };
        dl
    }

    #[test]
    fn dialog_progress_drain_failure_keeps_the_list() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut dl = running(rx);
        tx.send(hf::Progress::Failed("x".into())).unwrap();
        let action = dl.drain().expect("a failure asks for a note");
        assert!(action.note.contains('x'));
        assert!(action.error && !action.reload && !action.close);
        assert!(matches!(dl.state, DownloadState::Idle));
        assert_eq!(dl.files.len(), 1, "the pick survives a failed download");
    }

    #[test]
    fn dialog_progress_drain_keeps_running_when_queue_empty() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut dl = running(rx);
        tx.send(hf::Progress::Downloading { bytes: 5, total: 9 }).unwrap();
        assert!(dl.drain().is_none(), "progress alone asks for nothing");
        assert!(matches!(
            dl.state,
            DownloadState::Running { bytes: 5, total: 9, .. }
        ));
    }

    #[test]
    fn dialog_progress_drain_cancel_returns_to_idle() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut dl = running(rx);
        tx.send(hf::Progress::Cancelled).unwrap();
        let action = dl.drain().expect("a cancel asks for a note");
        assert!(!action.error && !action.reload && !action.close);
        assert!(matches!(dl.state, DownloadState::Idle));
        assert_eq!(dl.files.len(), 1);
    }

    #[test]
    fn dialog_progress_drain_done_reloads_and_closes() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut dl = running(rx);
        tx.send(hf::Progress::Done { folder: "m".into(), toml: "m/m.toml".into() })
            .unwrap();
        let action = dl.drain().expect("success asks for a reload");
        assert!(action.reload && action.close && !action.error);
        assert!(matches!(dl.state, DownloadState::Idle));
    }

    fn picker(
        id: &str,
        text: &str,
        unit: &str,
        value: f64,
        hardware: &[&str],
        hardware_id: &str,
        group: &str,
    ) -> PickerSensor {
        PickerSensor {
            id: id.into(),
            text: text.into(),
            unit: unit.into(),
            value,
            hardware: hardware.iter().map(|h| (*h).to_owned()).collect(),
            hardware_id: hardware_id.into(),
            group: group.into(),
        }
    }

    fn sample_sensor() -> PickerSensor {
        picker(
            "lhm:/gpu/0/temperature/0",
            "GPU Core",
            "°C",
            40.0,
            &["GPU"],
            "/gpu/0",
            "Temperatures",
        )
    }

    #[test]
    fn hardware_category_maps_lhm_prefixes() {
        for (id, expected) in [
            ("/intelcpu/0", "CPU"),
            ("/amdcpu/0", "CPU"),
            ("/cpu/0", "CPU"),
            ("/gpu-nvidia/0", "GPU"),
            ("/nvidiagpu/0", "GPU"),
            ("/atigpu/0", "GPU"),
            ("/gpu-amd/0", "GPU"),
            ("/ram", "Memory"),
            ("/vram", "Memory"),
            ("/motherboard", "Board"),
            ("/lpc/nct6791d/0", "Board"),
            ("/nvme/3", "Storage"),
            ("/hdd/0", "Storage"),
            ("/ssd/2", "Storage"),
            ("/nic/x", "Network"),
            ("/other", "Other"),
        ] {
            assert_eq!(hardware_category(id), expected, "{id}");
        }
    }

    #[test]
    fn build_sensor_tree_groups_by_category() {
        let sensors = vec![
            picker(
                "lhm:/gpu-nvidia/0/temperature/2",
                "GPU Hot Spot",
                "°C",
                72.0,
                &["NVIDIA GeForce RTX 3090"],
                "/gpu-nvidia/0",
                "Temperatures",
            ),
            picker(
                "lhm:/intelcpu/0/temperature/4",
                "CPU Package",
                "°C",
                35.0,
                &["Intel Xeon E5-2696 v4"],
                "/intelcpu/0",
                "Temperatures",
            ),
            picker(
                "lhm:/intelcpu/0/power/0",
                "CPU Package",
                "W",
                63.3,
                &["Intel Xeon E5-2696 v4"],
                "/intelcpu/0",
                "Powers",
            ),
        ];

        let tree = build_sensor_tree(&sensors, "");
        assert_eq!(
            tree.iter().map(|c| c.name).collect::<Vec<_>>(),
            vec!["CPU", "GPU"]
        );
        assert_eq!(tree[0].hardware.len(), 1);
        assert_eq!(tree[0].hardware[0].path, vec!["Intel Xeon E5-2696 v4"]);
        assert_eq!(tree[0].hardware[0].groups[0].name, "Temperatures");
        assert_eq!(tree[0].hardware[0].groups[0].sensors, vec![1]);
        assert_eq!(tree[0].hardware[0].groups[1].name, "Powers");
        assert_eq!(tree[0].hardware[0].groups[1].sensors, vec![2]);
        assert_eq!(tree[1].hardware[0].groups[0].sensors, vec![0]);
    }

    #[test]
    fn build_sensor_tree_filters_by_parent_names() {
        let sensors = vec![
            picker(
                "lhm:/gpu-nvidia/0/temperature/2",
                "GPU Hot Spot",
                "°C",
                72.0,
                &["NVIDIA GeForce RTX 3090"],
                "/gpu-nvidia/0",
                "Temperatures",
            ),
            picker(
                "lhm:/intelcpu/0/power/0",
                "CPU Package",
                "W",
                63.3,
                &["Intel Xeon E5-2696 v4"],
                "/intelcpu/0",
                "Powers",
            ),
        ];

        let by_hardware = build_sensor_tree(&sensors, "nvidia");
        assert_eq!(by_hardware.len(), 1);
        assert_eq!(by_hardware[0].name, "GPU");

        let by_group = build_sensor_tree(&sensors, "powers");
        assert_eq!(by_group.len(), 1);
        assert_eq!(by_group[0].hardware[0].groups[0].name, "Powers");

        let by_sensor = build_sensor_tree(&sensors, "cpu package");
        assert_eq!(by_sensor.len(), 1);
        assert_eq!(by_sensor[0].hardware[0].groups[0].sensors, vec![1]);
    }

    #[test]
    fn build_sensor_tree_falls_back_for_empty_hardware() {
        let sensors = vec![picker("lhm:/x", "Mystery", "", 1.0, &[], "", "")];
        let tree = build_sensor_tree(&sensors, "");
        assert_eq!(tree[0].name, "Other");
        assert_eq!(tree[0].hardware[0].path, vec!["Other"]);
        assert_eq!(tree[0].hardware[0].groups[0].name, "Other");
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
        let sensor = picker(
            "lhm:/gpu/0/temperature/1",
            "GPU Memory Junction Temperature",
            "°C",
            50.0,
            &[],
            "",
            "",
        );
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

    #[test]
    fn tray_menu_has_the_four_items() {
        let ids: Vec<_> = tray_menu_items(false).iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [TRAY_SHOW, TRAY_HIDE, TRAY_MONITOR, TRAY_EXIT]);
        let labels: Vec<_> = tray_menu_items(false).iter().map(|(_, l)| *l).collect();
        assert_eq!(
            labels,
            ["Show QuantDock", "Hide QuantDock", "Start monitor", "Exit"]
        );
        // The overlay label flips; the id it reports does not.
        assert_eq!(tray_menu_items(true)[2], (TRAY_MONITOR, "Close monitor"));
    }

    #[test]
    fn tooltip_text_matches_state() {
        assert_eq!(
            tooltip_text(Some(("qwen", "Qwen3 8B")), true),
            "QuantDock — serving qwen — Qwen3 8B · monitor up"
        );
        assert_eq!(
            tooltip_text(Some(("qwen", "Qwen3 8B")), false),
            "QuantDock — serving qwen — Qwen3 8B · monitor down"
        );
        assert_eq!(
            tooltip_text(None, true),
            "QuantDock — server down · monitor up"
        );
        assert_eq!(
            tooltip_text(None, false),
            "QuantDock — server down · monitor down"
        );
    }

    #[test]
    fn tray_icon_buffer_has_a_transparent_corner_an_opaque_centre_and_a_letter() {
        let size = 32u32;
        let px = tray_icon_rgba(size);
        assert_eq!(px.len(), (size * size * 4) as usize);
        assert_eq!(px[3], 0, "the rounded corner is transparent");
        let centre = ((size / 2) * size + size / 2) as usize * 4;
        assert_eq!(px[centre + 3], 255, "the icon body is opaque");
        assert!(
            px.chunks_exact(4)
                .any(|p| p[3] == 255 && p[..3].iter().all(|v| *v > 200)),
            "the Q is drawn in near-white"
        );
    }

    #[test]
    fn hide_bootstrap_re_sends_until_three_passes_and_reveal_cancels() {
        let (mut pending, mut passes) = (true, 0u32);
        let mut hidden = false;
        for _ in 0..HIDE_PASSES {
            let (send, next_pending, next_hidden) = hide_tick(pending, passes);
            assert!(send, "a pending hide sends Visible(false) on every pass");
            pending = next_pending;
            hidden = next_hidden;
            passes += 1;
        }
        assert!(!pending, "the bootstrap must stop after HIDE_PASSES");
        assert!(hidden, "the last pass owns the hide");
        // A reveal zeroes `hide_pending`, so no later pass may hide again.
        assert_eq!(hide_tick(false, passes), (false, false, false));
    }

    #[test]
    fn show_event_leaves_the_window_visible_and_the_bootstrap_cancelled() {
        let ctx = egui::Context::default();
        // Mid-bootstrap, two passes already sent.
        let (mut hidden, mut hide_pending, mut hide_passes) = (true, true, 2u32);
        // Tray Show and the second-instance event both go through `reveal`.
        reveal(
            &mut hidden,
            &mut hide_pending,
            &mut hide_passes,
            &ctx,
        );
        assert!(!hidden, "a show event makes the window visible");
        assert!(
            !hide_pending,
            "a show event must cancel the startup hide bootstrap"
        );
        assert_eq!(hide_passes, 0);
        // The next `logic()` pass must not hide the window again.
        assert_eq!(hide_tick(hide_pending, hide_passes), (false, false, false));
    }
}
