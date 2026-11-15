//! Model for the configurable monitor's `settings.toml`.

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How a field's value is formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Percent,
    Number,
    Gb,
    Celsius,
    Watts,
    Mhz,
    Megabytes,
    Rpm,
    Count,
}

#[derive(Debug, Clone, Copy)]
pub struct FieldDef {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
}

pub const FIELDS: &[FieldDef] = &[
    FieldDef { id: "model", label: "", kind: FieldKind::Text },
    FieldDef { id: "alias", label: "", kind: FieldKind::Text },
    FieldDef { id: "ctx", label: "ctx", kind: FieldKind::Text },
    FieldDef { id: "gpu", label: "GPU", kind: FieldKind::Percent },
    FieldDef { id: "vram", label: "VRAM", kind: FieldKind::Gb },
    FieldDef { id: "vram_pct", label: "VRAM", kind: FieldKind::Percent },
    FieldDef { id: "vram_bus", label: "BUS", kind: FieldKind::Percent },
    FieldDef { id: "gpu_temp", label: "GPU", kind: FieldKind::Celsius },
    FieldDef { id: "fan", label: "FAN", kind: FieldKind::Percent },
    FieldDef { id: "fan_rpm", label: "FAN", kind: FieldKind::Rpm },
    FieldDef { id: "power", label: "GPU PWR", kind: FieldKind::Watts },
    FieldDef { id: "clock", label: "CLOCK", kind: FieldKind::Mhz },
    FieldDef { id: "pstate", label: "PSTATE", kind: FieldKind::Text },
    FieldDef { id: "gpu_hotspot", label: "HOTSPOT", kind: FieldKind::Celsius },
    FieldDef { id: "gpu_mem_temp", label: "MEM", kind: FieldKind::Celsius },
    FieldDef { id: "gpu_shared", label: "SHARED", kind: FieldKind::Megabytes },
    FieldDef { id: "cpu", label: "CPU", kind: FieldKind::Percent },
    FieldDef { id: "cpu_peak", label: "PEAK", kind: FieldKind::Percent },
    FieldDef { id: "cpu_hot", label: "HOT", kind: FieldKind::Count },
    FieldDef { id: "cpu_temp", label: "CPU", kind: FieldKind::Celsius },
    FieldDef { id: "cpu_power", label: "CPU PWR", kind: FieldKind::Watts },
    FieldDef { id: "ram", label: "RAM", kind: FieldKind::Gb },
    FieldDef { id: "ram_pct", label: "RAM", kind: FieldKind::Percent },
    FieldDef { id: "swap", label: "swap", kind: FieldKind::Gb },
    FieldDef { id: "llama_ram", label: "LLAMA RAM", kind: FieldKind::Megabytes },
    FieldDef { id: "disk_free", label: "disk", kind: FieldKind::Gb },
    FieldDef { id: "uptime", label: "UP", kind: FieldKind::Text },
    FieldDef { id: "port", label: "port", kind: FieldKind::Text },
    FieldDef { id: "board_temp", label: "BOARD", kind: FieldKind::Celsius },
];

pub fn field_def(id: &str) -> Option<&'static FieldDef> {
    FIELDS.iter().find(|f| f.id == id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Text,
    Bar,
    Graph,
    GraphText,
}

impl Default for Style {
    fn default() -> Self {
        Self::Text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Above,
    Below,
}

impl Default for Direction {
    fn default() -> Self {
        Self::Above
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
}

impl Default for Corner {
    fn default() -> Self {
        Self::TopRight
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Threshold {
    pub warn: Option<f64>,
    pub crit: Option<f64>,
    pub direction: Direction,
}

impl Default for Threshold {
    fn default() -> Self {
        Self {
            warn: None,
            crit: None,
            direction: Direction::Above,
        }
    }
}

fn threshold(warn: f64, crit: f64) -> Threshold {
    Threshold {
        warn: Some(warn),
        crit: Some(crit),
        direction: Direction::Above,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Row {
    pub fields: Vec<String>,
}

/// Hex `#rrggbb` serialisation for `Color32`.
mod hex_color {
    use eframe::egui::Color32;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(c: &Color32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::to_hex(*c))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color32, D::Error> {
        let text = String::deserialize(d)?;
        super::parse_hex(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid colour: {text}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    #[serde(with = "hex_color")]
    pub text: Color32,
    #[serde(with = "hex_color")]
    pub label: Color32,
    #[serde(with = "hex_color")]
    pub accent: Color32,
    #[serde(with = "hex_color")]
    pub warn: Color32,
    #[serde(with = "hex_color")]
    pub crit: Color32,
    #[serde(with = "hex_color")]
    pub background: Color32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            text: Color32::from_rgb(0xd6, 0xd6, 0xd6),
            label: Color32::from_rgb(0x8a, 0x8a, 0x8a),
            accent: Color32::from_rgb(0x78, 0xbe, 0xff),
            warn: Color32::from_rgb(0xe0, 0xb0, 0x50),
            crit: Color32::from_rgb(0xe0, 0x78, 0x78),
            background: Color32::BLACK,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorSettings {
    /// Logical px; 0 = fit the widest row.
    pub width: f32,
    pub interval_ms: u64,
    pub background_alpha: u8,
    pub corner_radius: f32,
    pub font_size: f32,
    pub show_labels: bool,
    pub always_on_top: bool,
    pub show_close: bool,
    pub position: Corner,
    pub margin: f32,
    pub graph_samples: usize,
    pub title: String,
    /// User-picked sensors: `lhm:<SensorId>` -> short label.
    pub custom: BTreeMap<String, String>,
    pub rows: Vec<Row>,
    pub field_style: BTreeMap<String, Style>,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            width: 0.0,
            interval_ms: 1000,
            background_alpha: 190,
            // 0 by default: a rounded corner on a transparent window is a window onto
            // whatever is behind it, which reads as a stray floating widget.
            corner_radius: 0.0,
            font_size: 13.0,
            show_labels: true,
            always_on_top: true,
            show_close: true,
            position: Corner::TopRight,
            margin: 16.0,
            graph_samples: 60,
            title: String::new(),
            custom: BTreeMap::new(),
            rows: vec![
                Row { fields: vec!["model".into(), "uptime".into(), "llama_ram".into()] },
                Row { fields: vec!["gpu".into(), "vram".into(), "gpu_temp".into(), "power".into()] },
                Row { fields: vec!["gpu_hotspot".into(), "gpu_mem_temp".into(), "gpu_shared".into(), "fan".into()] },
                Row { fields: vec!["cpu".into(), "cpu_peak".into(), "cpu_hot".into(), "cpu_temp".into(), "cpu_power".into()] },
                Row { fields: vec!["ram".into(), "board_temp".into()] },
            ],
            field_style: BTreeMap::from([
                ("gpu".to_owned(), Style::Bar),
                ("cpu".to_owned(), Style::GraphText),
            ]),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SensorsSettings {
    /// Master switch for the optional LibreHardwareMonitor integration.
    pub enabled: bool,
    /// Where LibreHardwareMonitor.exe lives, when discovered or chosen by the user.
    pub path: Option<String>,
    /// How often the sensor JSON is fetched. Temperatures do not need 1 Hz.
    pub interval_ms: u64,
}

impl Default for SensorsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            path: None,
            interval_ms: 2000,
        }
    }
}

/// Field ids that were renamed after the first release.
const MIGRATIONS: &[(&str, &str)] = &[("temp", "gpu_temp"), ("gpu_mem", "vram_bus")];

fn migrate(cfg: &mut Config) {
    for row in &mut cfg.monitor.rows {
        for id in &mut row.fields {
            if let Some((_, to)) = MIGRATIONS.iter().find(|(from, _)| from == id) {
                *id = (*to).to_owned();
            }
        }
    }
    for (from, to) in MIGRATIONS {
        if let Some(style) = cfg.monitor.field_style.remove(*from) {
            cfg.monitor.field_style.insert((*to).to_owned(), style);
        }
        if let Some(t) = cfg.thresholds.remove(*from) {
            cfg.thresholds.insert((*to).to_owned(), t);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub monitor: MonitorSettings,
    pub thresholds: BTreeMap<String, Threshold>,
    pub theme: Theme,
    pub sensors: SensorsSettings,
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults()
    }
}

pub const HEADER: &str = "\
# QuantDock settings. Edited by the app; comments are lost when the UI saves.
# Hand edits are picked up while it runs, but a malformed file is never overwritten.
";

impl Config {
    pub fn defaults() -> Self {
        let mut thresholds = BTreeMap::new();
        thresholds.insert("gpu_temp".to_owned(), threshold(70.0, 82.0));
        thresholds.insert("cpu_temp".to_owned(), threshold(70.0, 85.0));
        thresholds.insert("gpu_hotspot".to_owned(), threshold(85.0, 95.0));
        thresholds.insert("gpu".to_owned(), threshold(90.0, 98.0));
        thresholds.insert("cpu_peak".to_owned(), threshold(80.0, 95.0));
        thresholds.insert("vram_pct".to_owned(), threshold(90.0, 97.0));
        thresholds.insert(
            "disk_free".to_owned(),
            Threshold {
                warn: Some(100.0),
                crit: Some(40.0),
                direction: Direction::Below,
            },
        );
        Self {
            monitor: MonitorSettings::default(),
            thresholds,
            theme: Theme::default(),
            sensors: SensorsSettings::default(),
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// `Ok(None)` when there is no file yet; `Err` when there is one but it cannot be
    /// used. Callers must not overwrite a file this returns `Err` for.
    pub fn load_existing(path: &Path) -> Result<Option<Self>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        if is_effectively_empty(text) {
            return Err("file is empty".to_owned());
        }
        let mut cfg: Config = toml_edit::de::from_str(text).map_err(|e| e.to_string())?;
        migrate(&mut cfg);
        cfg.monitor.interval_ms = cfg.monitor.interval_ms.clamp(250, 10_000);
        cfg.monitor.graph_samples = cfg.monitor.graph_samples.clamp(1, 600);
        cfg.monitor.width = cfg.monitor.width.clamp(0.0, 1200.0);
        cfg.monitor.font_size = cfg.monitor.font_size.clamp(8.0, 32.0);
        cfg.monitor.margin = cfg.monitor.margin.clamp(0.0, 400.0);
        cfg.monitor.corner_radius = cfg.monitor.corner_radius.clamp(0.0, 40.0);
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = toml_edit::ser::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Write beside the target, then rename, so a crash cannot truncate settings.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, format!("{HEADER}{body}"))?;
        std::fs::rename(&tmp, path)
    }

    /// Warnings for ids that are not in [`FIELDS`]. Unknown ids are kept on purpose:
    /// dropping them would silently delete a user's row while the app is open.
    pub fn validate(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut check = |id: &str| {
            if field_def(id).is_none() {
                warnings.push(format!("unknown field id: {id}"));
            }
        };
        for row in &self.monitor.rows {
            for id in &row.fields {
                check(id);
            }
        }
        for id in self.monitor.field_style.keys().chain(self.thresholds.keys()) {
            check(id);
        }
        warnings
    }
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("QuantDock");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.toml")
}

pub fn parse_hex(text: &str) -> Option<Color32> {
    let t = text.trim().trim_start_matches('#');
    if t.len() != 6 || !t.is_ascii() {
        return None;
    }
    let r = u8::from_str_radix(&t[0..2], 16).ok()?;
    let g = u8::from_str_radix(&t[2..4], 16).ok()?;
    let b = u8::from_str_radix(&t[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

/// True when the file has no actual TOML in it (empty, whitespace, or only comments).
fn is_effectively_empty(text: &str) -> bool {
    text.lines()
        .all(|l| l.trim().is_empty() || l.trim_start().starts_with('#'))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// Polls a file's mtime. The caller decides how often to ask; this never sleeps.
pub struct Watcher {
    pub path: PathBuf,
    last: Option<std::time::SystemTime>,
}

impl Watcher {
    pub fn new(path: PathBuf) -> Self {
        Self { path, last: None }
    }

    /// True once per observed change. The first call always reports the existing file,
    /// so callers can distinguish "loaded" from "nothing there".
    pub fn changed(&mut self) -> bool {
        match std::fs::metadata(&self.path).and_then(|m| m.modified()) {
            Ok(stamp) => {
                if self.last == Some(stamp) {
                    false
                } else {
                    self.last = Some(stamp);
                    true
                }
            }
            // Missing or unreadable is not a change: the overlay keeps its last config.
            Err(_) => {
                self.last = None;
                false
            }
        }
    }
}

pub fn window_path() -> PathBuf {
    data_dir().join("window.toml")
}

#[derive(Serialize, Deserialize)]
struct WindowPos {
    x: f32,
    y: f32,
}

pub fn load_window_pos() -> Option<eframe::egui::Pos2> {
    let text = std::fs::read_to_string(window_path()).ok()?;
    let p: WindowPos = toml_edit::de::from_str(&text).ok()?;
    Some(eframe::egui::pos2(p.x, p.y))
}

pub fn save_window_pos(pos: eframe::egui::Pos2) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir())?;
    std::fs::write(window_path(), format!("x = {}\ny = {}\n", pos.x, pos.y))
}

pub fn clear_window_pos() {
    let _ = std::fs::remove_file(window_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_fires_once_per_change() {
        let dir = std::env::temp_dir().join(format!("qd_watch_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.toml");
        std::fs::write(&p, "a = 1").unwrap();
        let mut w = Watcher::new(p.clone());
        assert!(w.changed(), "first poll must report the existing file");
        assert!(!w.changed(), "unchanged file must not fire twice");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&p, "a = 2").unwrap();
        assert!(w.changed());
    }

    #[test]
    fn watcher_survives_a_deleted_file() {
        let dir = std::env::temp_dir().join(format!("qd_watch_del_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.toml");
        std::fs::write(&p, "a = 1").unwrap();
        let mut w = Watcher::new(p.clone());
        w.changed();
        std::fs::remove_file(&p).unwrap();
        assert!(!w.changed(), "missing file is not a change and must not panic");
    }

    #[test]
    #[ignore = "touches the real %APPDATA%"]
    fn window_pos_round_trips() {
        let pos = eframe::egui::pos2(1234.0, 56.0);
        save_window_pos(pos).unwrap();
        assert_eq!(load_window_pos(), Some(pos));
        clear_window_pos();
        assert_eq!(load_window_pos(), None);
    }

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::defaults();
        let ids: Vec<Vec<&str>> = c
            .monitor
            .rows
            .iter()
            .map(|r| r.fields.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(
            ids,
            vec![
                vec!["model", "uptime", "llama_ram"],
                vec!["gpu", "vram", "gpu_temp", "power"],
                vec!["gpu_hotspot", "gpu_mem_temp", "gpu_shared", "fan"],
                vec!["cpu", "cpu_peak", "cpu_hot", "cpu_temp", "cpu_power"],
                vec!["ram", "board_temp"],
            ]
        );
        assert_eq!(c.monitor.field_style.get("gpu"), Some(&Style::Bar));
        assert_eq!(c.thresholds["gpu_temp"].warn, Some(70.0));
        assert_eq!(c.thresholds["disk_free"].direction, Direction::Below);
    }

    #[test]
    fn migrates_old_field_ids() {
        let cfg = Config::parse("[[monitor.rows]]\nfields = [\"temp\", \"gpu_mem\"]\n").unwrap();
        assert_eq!(
            cfg.monitor.rows[0].fields,
            vec!["gpu_temp".to_string(), "vram_bus".to_string()]
        );
    }

    #[test]
    fn new_fields_have_labels_and_kinds() {
        assert_eq!(field_def("cpu_power").unwrap().label, "CPU PWR");
        assert_eq!(field_def("power").unwrap().label, "GPU PWR");
        assert_eq!(field_def("gpu_hotspot").unwrap().kind, FieldKind::Celsius);
        assert_eq!(field_def("fan_rpm").unwrap().kind, FieldKind::Rpm);
        assert_eq!(field_def("vram_bus").unwrap().label, "BUS");
    }

    #[test]
    fn round_trips_through_toml() {
        let mut c = Config::defaults();
        c.monitor.width = 0.0;
        c.theme.crit = Color32::from_rgb(1, 2, 3);
        let text = toml_edit::ser::to_string_pretty(&c).unwrap();
        let back: Config = toml_edit::de::from_str(&text).unwrap();
        assert_eq!(back.monitor.width, 0.0);
        assert_eq!(back.theme.crit, Color32::from_rgb(1, 2, 3));
        assert_eq!(back.monitor.rows.len(), c.monitor.rows.len());
    }

    #[test]
    fn malformed_file_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("qd_cfg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.toml");
        std::fs::write(&p, "this is not toml [[[").unwrap();
        assert!(Config::load(&p).is_err());
    }

    #[test]
    fn hex_colours_round_trip() {
        let c = Color32::from_rgb(18, 52, 86);
        assert_eq!(parse_hex(&to_hex(c)), Some(c));
        assert_eq!(parse_hex("#123456"), Some(c));
        assert_eq!(parse_hex("nope"), None);
        // A six-byte non-ASCII value must not panic on char-boundary slicing.
        assert_eq!(parse_hex("€aaa"), None);
    }

    #[test]
    fn empty_and_comment_only_files_are_errors() {
        let dir = std::env::temp_dir().join(format!("qd_empty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for body in ["", "   \n\t\n", "# just a comment\n"] {
            let p = dir.join("settings.toml");
            std::fs::write(&p, body).unwrap();
            assert!(Config::load(&p).is_err(), "should reject {body:?}");
        }
    }

    #[test]
    fn load_existing_separates_missing_from_broken() {
        let dir = std::env::temp_dir().join(format!("qd_missing_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.toml");
        let _ = std::fs::remove_file(&p);
        assert!(matches!(Config::load_existing(&p), Ok(None)));
        std::fs::write(&p, "broken [[[").unwrap();
        assert!(Config::load_existing(&p).is_err());
        std::fs::write(&p, "[monitor]\nwidth = 300.0\n").unwrap();
        assert!(matches!(Config::load_existing(&p), Ok(Some(_))));
    }

    #[test]
    fn validate_reports_unknown_fields_but_keeps_them() {
        let mut c = Config::defaults();
        c.monitor.rows[1].fields.push("gpu_temp_magic".into());
        let warnings = c.validate();
        assert!(warnings.iter().any(|w| w.contains("gpu_temp_magic")));
        assert!(c.monitor.rows[1]
            .fields
            .contains(&"gpu_temp_magic".to_string()));
    }
}
