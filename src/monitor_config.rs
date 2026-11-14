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
    FieldDef { id: "gpu_mem", label: "VRAM IO", kind: FieldKind::Percent },
    FieldDef { id: "temp", label: "", kind: FieldKind::Celsius },
    FieldDef { id: "fan", label: "fan", kind: FieldKind::Percent },
    FieldDef { id: "power", label: "", kind: FieldKind::Watts },
    FieldDef { id: "clock", label: "", kind: FieldKind::Mhz },
    FieldDef { id: "pstate", label: "", kind: FieldKind::Text },
    FieldDef { id: "cpu", label: "CPU", kind: FieldKind::Percent },
    FieldDef { id: "cpu_peak", label: "peak", kind: FieldKind::Percent },
    FieldDef { id: "cpu_hot", label: "hot", kind: FieldKind::Count },
    FieldDef { id: "ram", label: "RAM", kind: FieldKind::Gb },
    FieldDef { id: "ram_pct", label: "RAM", kind: FieldKind::Percent },
    FieldDef { id: "swap", label: "swap", kind: FieldKind::Gb },
    FieldDef { id: "llama_ram", label: "llama", kind: FieldKind::Number },
    FieldDef { id: "disk_free", label: "disk", kind: FieldKind::Gb },
    FieldDef { id: "uptime", label: "", kind: FieldKind::Text },
    FieldDef { id: "port", label: "port", kind: FieldKind::Text },
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
    pub rows: Vec<Row>,
    pub field_style: BTreeMap<String, Style>,
}

impl Default for MonitorSettings {
    fn default() -> Self {
        Self {
            width: 320.0,
            interval_ms: 1000,
            background_alpha: 190,
            corner_radius: 12.0,
            font_size: 13.0,
            show_labels: true,
            always_on_top: true,
            show_close: true,
            position: Corner::TopRight,
            margin: 16.0,
            graph_samples: 60,
            rows: vec![
                Row { fields: vec!["model".into()] },
                Row { fields: vec!["gpu".into(), "vram".into()] },
                Row { fields: vec!["cpu".into(), "cpu_peak".into(), "cpu_hot".into()] },
                Row { fields: vec!["temp".into(), "fan".into(), "power".into()] },
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
pub struct Config {
    pub monitor: MonitorSettings,
    pub thresholds: BTreeMap<String, Threshold>,
    pub theme: Theme,
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
        thresholds.insert("temp".to_owned(), threshold(70.0, 82.0));
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
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut cfg: Config =
            toml_edit::de::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.monitor.interval_ms = cfg.monitor.interval_ms.clamp(250, 10_000);
        cfg.monitor.graph_samples = cfg.monitor.graph_samples.clamp(1, 600);
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
    if t.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&t[0..2], 16).ok()?;
    let g = u8::from_str_radix(&t[2..4], 16).ok()?;
    let b = u8::from_str_radix(&t[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

pub fn to_hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                vec!["model"],
                vec!["gpu", "vram"],
                vec!["cpu", "cpu_peak", "cpu_hot"],
                vec!["temp", "fan", "power"],
            ]
        );
        assert_eq!(c.monitor.field_style.get("gpu"), Some(&Style::Bar));
        assert_eq!(c.thresholds["temp"].warn, Some(70.0));
        assert_eq!(c.thresholds["disk_free"].direction, Direction::Below);
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
