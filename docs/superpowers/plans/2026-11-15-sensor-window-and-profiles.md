# Sensor Manager Window, Profiles, and Left-Aligned Text Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the inline sensor picker with one complete sensor-manager window, give custom sensors the regular label/style/threshold controls, add ten full-overlay profile slots with a Load anyway/Cancel confirmation, and keep all body text left-aligned.

**Architecture:** Add a `Profile` snapshot to `monitor_config`, route every field through a borrowed `FieldView` resolver in `monitor_panel`, and keep the Settings draft as the only source of truth. Two in-app `egui::Window`s (sensor manager, profile confirmation) sit over the existing manager; no second viewport, no new crate, no new sampler path.

**Tech Stack:** Rust 2024, egui/eframe 0.37, toml_edit, serde, LibreHardwareMonitor HTTP via the existing `Reader`.

**Spec:** `docs/superpowers/specs/2026-11-15-sensor-window-and-profiles-design.md`

## Global Constraints

- Windows-only. No cross-platform fallbacks and no new dependencies.
- Both binaries stay GUI-subsystem; child processes use `quantdock::hidden()`.
- No separate OS windows. The sensor manager and profile confirmation are in-app `egui::Window`s.
- Sensors settings (`enabled`, `path`, `interval_ms`, `autostart`) are not part of a profile.
- Unchecking a field removes it from every row but keeps its label, style and thresholds. Only `Forget` deletes stored custom config.
- Custom sensors support text and bar only. Graph styles stay built-in-only. Bar is offered only when the id contains `/load/`.
- Loading a profile when the current snapshot matches no filled slot offers only **Load anyway** and **Cancel**.
- Body text stays left-aligned. `Layout::right_to_left` is only for action clusters; only the overlay close glyph and text drawn inside a bar may be centred.
- The old inline picker is removed, but the unidentified crash is not claimed fixed. The panic hook stays, and `%APPDATA%\QuantDock\crash.log` is the evidence path.
- A malformed `settings.toml` is never overwritten. Profiles save through the same atomic path as the rest of the config.
- Baseline before this plan: 73 tests (65 lib + 8 bin), 4 ignored. `cargo test --all-targets` and `cargo build --release` must stay green.

## File Structure

| File | Responsibility |
|---|---|
| `src/monitor_config.rs` | `Profile`, `Config.profiles`, snapshot/apply/match helpers, `PartialEq` derives, profile TOML tests. |
| `src/monitor_panel.rs` | `FieldView` resolver, `custom_kind`, unified field drawing and width, custom bar rendering. |
| `src/bin/quantdock.rs` | Sensor-manager window, custom field table, profile grid/dialogs, field helpers, left-alignment wiring. |
| `README.md` | User-facing sensor window, custom fields and profiles. |
| `INSTALL.md` | Short setup note for the sensor window and profiles. |
| `AGENTS.md` | Current spec pointer, code map, current state, test count. |

---

### Task 1: Profile snapshot in the config model

**Files:**
- Modify: `src/monitor_config.rs`

**Interfaces:**
- Produces: `pub struct Profile { pub monitor: MonitorSettings, pub thresholds: BTreeMap<String, Threshold>, pub theme: Theme }`, deriving `Debug, Clone, Default, PartialEq, Serialize, Deserialize`.
- Produces: `Config.profiles: BTreeMap<String, Profile>`, keys `"1"`–`"10"`.
- Produces: `Config::profile(&self, slot: usize) -> Option<&Profile>`
- Produces: `Config::snapshot(&self) -> Profile`
- Produces: `Config::set_profile(&mut self, slot: usize)`
- Produces: `Config::apply_profile(&mut self, profile: &Profile)`
- Produces: `Config::matches_a_profile(&self) -> bool`
- Produces: `PartialEq` on `MonitorSettings`, `Threshold` and `Row` (required by `Profile`).

- [ ] **Step 1: Write the failing tests**

Append these tests inside `mod tests` in `src/monitor_config.rs`, after `round_trips_through_toml`:

```rust
#[test]
fn profile_round_trips_through_toml() {
    let mut c = Config::defaults();
    let mut p = Profile::default();
    p.monitor.width = 420.0;
    p.monitor.rows = vec![Row {
        fields: vec!["gpu_temp".into()],
    }];
    p.thresholds.insert(
        "gpu_temp".into(),
        Threshold {
            warn: Some(70.0),
            crit: Some(82.0),
            direction: Direction::Above,
        },
    );
    p.theme.accent = Color32::from_rgb(1, 2, 3);
    c.profiles.insert("1".into(), p);

    let text = toml_edit::ser::to_string_pretty(&c).unwrap();
    assert!(text.contains("[profiles.1]"), "{text}");
    assert!(text.contains("[[profiles.1.monitor.rows]]"), "{text}");
    assert!(text.contains("[profiles.1.thresholds.gpu_temp]"), "{text}");
    assert!(text.contains("[profiles.1.theme]"), "{text}");

    let back: Config = toml_edit::de::from_str(&text).unwrap();
    assert_eq!(back.profiles, c.profiles);
}

#[test]
fn save_profile_snapshots_the_full_overlay_config() {
    let mut c = Config::defaults();
    c.monitor.width = 480.0;
    c.monitor.custom.insert("lhm:/x".into(), "X".into());
    c.monitor.field_style.insert("gpu".into(), Style::Graph);
    c.theme.crit = Color32::from_rgb(9, 8, 7);

    c.set_profile(9);
    let p = c.profile(9).expect("slot 10 must exist");

    assert_eq!(p.monitor.width, 480.0);
    assert_eq!(p.monitor.custom["lhm:/x"], "X");
    assert_eq!(p.monitor.field_style["gpu"], Style::Graph);
    assert_eq!(p.theme.crit, Color32::from_rgb(9, 8, 7));
    assert_eq!(p.monitor.rows, c.monitor.rows);
    assert_eq!(p.thresholds, c.thresholds);
}

#[test]
fn applying_a_profile_keeps_sensor_settings() {
    let mut c = Config::defaults();
    c.sensors.enabled = false;
    c.sensors.path = Some("C:\\Tools\\LibreHardwareMonitor.exe".into());
    c.sensors.interval_ms = 500;
    c.sensors.autostart = true;

    let mut p = Profile::default();
    p.monitor.width = 333.0;
    p.theme.warn = Color32::from_rgb(4, 5, 6);
    c.apply_profile(&p);

    assert_eq!(c.monitor.width, 333.0);
    assert_eq!(c.theme.warn, Color32::from_rgb(4, 5, 6));
    assert!(!c.sensors.enabled);
    assert_eq!(
        c.sensors.path.as_deref(),
        Some("C:\\Tools\\LibreHardwareMonitor.exe")
    );
    assert_eq!(c.sensors.interval_ms, 500);
    assert!(c.sensors.autostart);
}

#[test]
fn profile_matches_only_its_own_layout() {
    let mut c = Config::defaults();
    c.monitor.width = 410.0;
    c.set_profile(2);
    assert!(c.matches_a_profile());

    c.monitor.width = 411.0;
    assert!(!c.matches_a_profile());
}

#[test]
fn sensor_only_edits_do_not_dirty_a_profile() {
    let mut c = Config::defaults();
    c.set_profile(0);
    c.sensors.path = Some("C:\\Other\\LibreHardwareMonitor.exe".into());
    c.sensors.interval_ms = 500;
    c.sensors.enabled = true;
    assert!(c.matches_a_profile());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib monitor_config::tests::profile_round_trips_through_toml`

Expected: FAIL to compile with `cannot find type Profile in this scope` and `no field profiles on type Config`.

- [ ] **Step 3: Add the model and helpers**

In `src/monitor_config.rs`, add `PartialEq` to the three derives:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Threshold {
```

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Row {
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MonitorSettings {
```

Add `Profile` above `Config`:

```rust
/// One complete overlay setup. Machine sensor settings are deliberately absent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub monitor: MonitorSettings,
    pub thresholds: BTreeMap<String, Threshold>,
    pub theme: Theme,
}
```

Add the field to `Config`:

```rust
pub struct Config {
    pub monitor: MonitorSettings,
    pub thresholds: BTreeMap<String, Threshold>,
    pub theme: Theme,
    pub sensors: SensorsSettings,
    /// Profile slots, keyed "1".."10".
    pub profiles: BTreeMap<String, Profile>,
}
```

Add `profiles: BTreeMap::new(),` to `Config::defaults()`.

Add these methods inside the existing `impl Config` block, before `load`:

```rust
/// Slot is the zero-based grid index; the stored key is the visible number.
pub fn profile(&self, slot: usize) -> Option<&Profile> {
    self.profiles.get(&(slot + 1).to_string())
}

pub fn snapshot(&self) -> Profile {
    Profile {
        monitor: self.monitor.clone(),
        thresholds: self.thresholds.clone(),
        theme: self.theme,
    }
}

pub fn set_profile(&mut self, slot: usize) {
    let snapshot = self.snapshot();
    self.profiles.insert((slot + 1).to_string(), snapshot);
}

/// Replaces the overlay setup and leaves `sensors` untouched.
pub fn apply_profile(&mut self, profile: &Profile) {
    self.monitor = profile.monitor.clone();
    self.thresholds = profile.thresholds.clone();
    self.theme = profile.theme;
}

pub fn matches_a_profile(&self) -> bool {
    let current = self.snapshot();
    self.profiles.values().any(|profile| *profile == current)
}
```

- [ ] **Step 4: Run the config tests**

Run: `cargo test --lib monitor_config`

Expected: PASS. If `profile_round_trips_through_toml` prints a shape without `[profiles.1]`, do not change the storage decision; pin the assertion to the actual `[profiles."1"]` form only if `toml_edit` quotes the numeric key, and keep the round-trip assertion.

- [ ] **Step 5: Commit**

```bash
git add src/monitor_config.rs
git commit -m "feat(config): add ten overlay profile slots"
```

---

### Task 2: One `FieldView` resolver for built-in and custom fields

**Files:**
- Modify: `src/monitor_panel.rs`

**Interfaces:**
- Produces: `#[derive(Clone, Copy)] pub struct FieldView<'a> { pub id: &'a str, pub label: &'a str, pub kind: FieldKind }`
- Produces: `pub fn custom_kind(id: &str) -> FieldKind`
- Produces: `pub fn field_view<'a>(cfg: &'a Config, id: &'a str) -> Option<FieldView<'a>>`
- Produces: `pub fn visible_fields<'a>(cfg: &'a Config, row: &'a Row) -> Vec<FieldView<'a>>`
- Changes: `format_value`, `threshold_value`, `bar_fraction` and `draw_field` take `&FieldView`; `draw_custom` and `custom_row_width` are deleted.
- Sets the formatter for custom sensors: `/load/` is `Percent`, `/temperature/` is `Celsius`, `/power/` is `Watts`, `/clock/` is `Mhz`, `/fan/` is `Rpm`, everything else is `Number`.

- [ ] **Step 1: Write the failing tests**

Add these tests inside `mod tests` in `src/monitor_panel.rs`:

```rust
#[test]
fn custom_kind_infers_from_the_sensor_path() {
    assert_eq!(
        custom_kind("lhm:/gpu-nvidia/0/load/0"),
        FieldKind::Percent
    );
    assert_eq!(
        custom_kind("lhm:/gpu-nvidia/0/temperature/0"),
        FieldKind::Celsius
    );
    assert_eq!(custom_kind("lhm:/gpu-nvidia/0/power/0"), FieldKind::Watts);
    assert_eq!(custom_kind("lhm:/gpu-nvidia/0/clock/0"), FieldKind::Mhz);
    assert_eq!(custom_kind("lhm:/gpu-nvidia/0/fan/0"), FieldKind::Rpm);
    assert_eq!(custom_kind("lhm:/gpu-nvidia/0/voltage/0"), FieldKind::Number);
}

#[test]
fn custom_bar_uses_the_percent_scale_and_missing_is_dash() {
    let mut cfg = Config::defaults();
    let id = "lhm:/gpu-nvidia/0/load/0";
    cfg.monitor.custom.insert(id.into(), "GPU LOAD".into());
    let view = field_view(&cfg, id).unwrap();

    assert_eq!(bar_fraction(&view, Some(&Value::Num(50.0))), Some(0.5));
    assert!(bar_fraction(&view, None).is_none(), "a missing bar must not draw zero");
    assert_eq!(format_value(&view, None, &Sample::default()), "--");

    let volts = "lhm:/gpu-nvidia/0/voltage/0";
    cfg.monitor.custom.insert(volts.into(), "GPU V".into());
    let view = field_view(&cfg, volts).unwrap();
    assert_eq!(
        format_value(&view, Some(&Value::Num(1.23)), &Sample::default()),
        "1.23"
    );
}

#[test]
fn custom_width_matches_a_bar_style() {
    let mut cfg = Config::defaults();
    let id = "lhm:/gpu-nvidia/0/load/0";
    cfg.monitor.width = 0.0;
    cfg.monitor.show_labels = false;
    cfg.monitor.rows = vec![Row {
        fields: vec![id.into()],
    }];
    cfg.monitor.custom.insert(id.into(), "GPU LOAD".into());
    cfg.monitor.field_style.insert(id.into(), Style::Bar);

    let mut sample = Sample::default();
    sample.custom.insert(
        id.into(),
        crate::monitor_lhm::SensorValue {
            text: "GPU LOAD".into(),
            unit: "%".into(),
            value: 50.0,
        },
    );
    let measure = |t: &str, _size: f32| t.len() as f32 * 10.0;
    assert_eq!(
        natural_width(&cfg, &sample, &measure),
        24.0 + cfg.monitor.font_size * 7.0
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib monitor_panel::tests::custom_kind_infers_from_the_sensor_path`

Expected: FAIL to compile with `cannot find function custom_kind` and `cannot find function field_view`.

- [ ] **Step 3: Add the resolver**

At the top of `src/monitor_panel.rs`, replace the import with:

```rust
use crate::monitor_config::{
    field_def, Config, Direction, FieldKind, Row, Style, Threshold, Theme,
};
```

Replace `visible_fields` with:

```rust
/// A field as the panel renders it: built-in vocabulary or a stored custom sensor.
#[derive(Clone, Copy)]
pub struct FieldView<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub kind: FieldKind,
}

/// Custom sensors persist only an id, so the kind comes from the LHM path family.
pub fn custom_kind(id: &str) -> FieldKind {
    if id.contains("/load/") {
        FieldKind::Percent
    } else if id.contains("/temperature/") {
        FieldKind::Celsius
    } else if id.contains("/power/") {
        FieldKind::Watts
    } else if id.contains("/clock/") {
        FieldKind::Mhz
    } else if id.contains("/fan/") {
        FieldKind::Rpm
    } else {
        FieldKind::Number
    }
}

pub fn field_view<'a>(cfg: &'a Config, id: &'a str) -> Option<FieldView<'a>> {
    if let Some(def) = field_def(id) {
        return Some(FieldView {
            id: def.id,
            label: def.label,
            kind: def.kind,
        });
    }
    if !id.starts_with("lhm:") {
        return None;
    }
    let label = cfg
        .monitor
        .custom
        .get(id)
        .map(String::as_str)
        .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id));
    Some(FieldView {
        id,
        label,
        kind: custom_kind(id),
    })
}

pub fn visible_fields<'a>(cfg: &'a Config, row: &'a Row) -> Vec<FieldView<'a>> {
    row.fields
        .iter()
        .filter_map(|id| field_view(cfg, id.as_str()))
        .collect()
}
```

Change the three helpers to take a view:

```rust
pub fn format_value(view: &FieldView, value: Option<&Value>, _sample: &Sample) -> String {
    let Some(value) = value else {
        return "--".to_owned();
    };
    match (view.kind, value) {
```

```rust
fn threshold_value(view: &FieldView, value: Option<&Value>) -> Option<f64> {
    match (view.kind, value?) {
```

```rust
fn bar_fraction(view: &FieldView, value: Option<&Value>) -> Option<f64> {
    match (view.kind, value?) {
```

The bodies stay unchanged except for two details: `def.kind` becomes `view.kind`, and the
`Number` arm prints two decimals because custom numeric sensors use it (no built-in field
does):

```rust
        (FieldKind::Number, Value::Num(n)) => format!("{n:.2}"),
```

The matching placeholder in `natural_width` becomes `"999.99".to_owned()`.

- [ ] **Step 4: Draw every field through one path**

Add the shared style resolver before `draw_field`:

```rust
/// Text-only fields stay text; custom sensors have no history, so graph styles fall
/// back to text even if a hand-edited config asks for one.
fn style_for(cfg: &Config, view: &FieldView) -> Style {
    let style = cfg
        .monitor
        .field_style
        .get(view.id)
        .copied()
        .unwrap_or_default();
    if matches!(view.kind, FieldKind::Text)
        || (view.id.starts_with("lhm:") && matches!(style, Style::Graph | Style::GraphText))
    {
        Style::Text
    } else {
        style
    }
}
```

Replace the start of `draw_field` with:

```rust
fn draw_field(ui: &mut egui::Ui, cfg: &Config, view: &FieldView, sample: &Sample) {
    let custom_value = sample.custom.get(view.id).map(|s| Value::Num(s.value));
    let value = sample.fields.get(view.id).or(custom_value.as_ref());
    let shown = format_value(view, value, sample);
    let size = cfg.monitor.font_size;

    let value_color = if view.id == "model" || view.id == "alias" {
        cfg.theme.accent
    } else {
        threshold_value(view, value)
            .map(|n| color_for(cfg.thresholds.get(view.id), n, &cfg.theme))
            .unwrap_or(cfg.theme.text)
    };

    if cfg.monitor.show_labels && !view.label.is_empty() {
        ui.label(
            egui::RichText::new(view.label)
                .size(size)
                .color(cfg.theme.label),
        );
    }
```

Replace every remaining `def.id` with `view.id`, every `def.kind` with `view.kind`, and every `def.label` with `view.label`. Replace the whole style lookup (from `let style = cfg` through the `Style::Text` fallback) with:

```rust
    let style = style_for(cfg, view);
```

Delete `custom_row_width` and `draw_custom` entirely.

- [ ] **Step 5: Rewrite `natural_width` over views**

Replace the body of the row loop with:

```rust
    for row in &cfg.monitor.rows {
        let fields = visible_fields(cfg, row);
        if fields.is_empty() {
            continue;
        }
        let mut width = 0.0;
        for (i, view) in fields.iter().enumerate() {
            if i > 0 {
                width += GAP;
            }
            let style = style_for(cfg, view);
            let custom_value = sample.custom.get(view.id).map(|s| Value::Num(s.value));
            let value = sample.fields.get(view.id).or(custom_value.as_ref());
            let shown = match view.kind {
                FieldKind::Text => format_value(view, value, sample),
                FieldKind::Percent => "100%".to_owned(),
                FieldKind::Celsius => "100 °C".to_owned(),
                FieldKind::Watts => "999 W".to_owned(),
                FieldKind::Mhz => "9999 MHz".to_owned(),
                FieldKind::Gb => "99.9/99.9 GB".to_owned(),
                FieldKind::Megabytes => "9999 MB".to_owned(),
                FieldKind::Rpm => "9999 RPM".to_owned(),
                FieldKind::Number => "999.99".to_owned(),
                FieldKind::Count => "99".to_owned(),
            };

            if cfg.monitor.show_labels && !view.label.is_empty() {
                width += measure(view.label, size) + 12.0;
            }
            width += match style {
                Style::Bar if bar_fraction(view, value).is_some() => size * 7.0,
                Style::Graph => size * 9.0,
                Style::GraphText => size * 7.0 + 12.0 + measure(&shown, size),
                _ => measure(&shown, size),
            };
        }
        widest = widest.max(width);
    }
```

Delete the old `custom_row_width` call. The `PAD` and final `(widest + PAD).max(60.0)` stay.

- [ ] **Step 6: Update `render` and the existing tests**

In `render`, replace the spacer check and the draw loop with:

```rust
                let is_spacer = row
                    .fields
                    .iter()
                    .all(|id| field_view(cfg, id).is_none());
                if is_spacer {
                    ui.add_space(6.0);
                    continue;
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    for id in &row.fields {
                        if let Some(view) = field_view(cfg, id.as_str()) {
                            draw_field(ui, cfg, &view, sample);
                        }
                    }
                });
```

Update the existing tests that call the changed signatures:

```rust
    let cfg = Config::defaults();
    assert_eq!(
        format_value(field_view(&cfg, "gpu").unwrap(), s.fields.get("gpu"), &s),
        "15%"
    );
```

```rust
    assert_eq!(
        format_value(field_view(&cfg, "vram").unwrap(), s.fields.get("vram"), &s),
        "3.2/24.0 GB"
    );
```

```rust
    let cfg = Config::defaults();
    assert_eq!(
        format_value(field_view(&cfg, "gpu_temp").unwrap(), None, &s),
        "--"
    );
```

```rust
    let cfg = Config::defaults();
    let visible: Vec<&str> = visible_fields(&cfg, &row).iter().map(|d| d.id).collect();
```

```rust
    let cfg = Config::defaults();
    let view = field_view(&cfg, "disk_free").unwrap();
    // 50 GB free, expressed in the MB the sampler emits.
    let fifty = Some(Value::Num(50.0 * 1024.0));
    assert_eq!(threshold_value(&view, fifty.as_ref()), Some(50.0));
```

Use the same `view` pattern in `disk_free_threshold_is_read_in_gb`'s colour assertions and in `vram_bar_and_threshold_use_used_gb`. In `unscaled_fields_have_no_bar`, create one config and two views:

```rust
    let cfg = Config::defaults();
    assert_eq!(
        bar_fraction(
            &field_view(&cfg, "gpu_temp").unwrap(),
            Some(&Value::Num(52.0))
        ),
        None
    );
    assert_eq!(
        bar_fraction(
            &field_view(&cfg, "disk_free").unwrap(),
            Some(&Value::Num(50.0 * 1024.0))
        ),
        None
    );
```

- [ ] **Step 7: Run the panel and full test suite**

Run: `cargo test --lib monitor_panel`

Expected: PASS, including the three new tests and the updated existing ones.

Run: `cargo test --all-targets`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/monitor_panel.rs
git commit -m "refactor(panel): resolve built-in and custom fields through FieldView"
```

---

### Task 3: Shared field add/remove helpers

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Produces: `fn field_checked(cfg: &Config, id: &str) -> bool`
- Produces: `fn add_field_to_row(cfg: &mut Config, row: usize, id: &str) -> bool`
- Produces: `fn remove_field_everywhere(cfg: &mut Config, id: &str) -> bool`
- Produces: `fn forget_custom_field(cfg: &mut Config, id: &str)`
- Changes: `add_picked_sensor` uses `add_field_to_row`; `rows_ui`'s `+ field` menu and `Act::AddField` use the same helpers.

- [ ] **Step 1: Write the failing tests**

Replace `picker_add_does_not_duplicate_a_field` and add the new tests in `mod tests` in `src/bin/quantdock.rs`:

```rust
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
```

Add `Direction` and `Style` to the test module imports:

```rust
use quantdock::monitor_config::{Direction, Row, Style, Threshold};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin quantdock add_field_to_row_scans_every_row`

Expected: FAIL to compile with `cannot find function add_field_to_row`.

- [ ] **Step 3: Add the helpers**

Replace `add_picked_sensor` in `src/bin/quantdock.rs` with:

```rust
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

fn add_picked_sensor(cfg: &mut Config, row: usize, sensor: &PickerSensor) -> bool {
    if !add_field_to_row(cfg, row, &sensor.id) {
        return false;
    }
    let label: String = sensor.text.to_uppercase().chars().take(12).collect();
    cfg.monitor.custom.entry(sensor.id.clone()).or_insert(label);
    true
}
```

- [ ] **Step 4: Route the row builder through the helpers**

In `rows_ui`, change the `+ field` menu filter:

```rust
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
```

Change `Act::AddField` to:

```rust
                Act::AddField(i, id) => {
                    add_field_to_row(&mut self.cfg, i, &id);
                }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --all-targets`

Expected: PASS. The old `picker_add_does_not_duplicate_a_field` is replaced; `picker_label_is_uppercase_and_short` still passes through `add_picked_sensor`.

- [ ] **Step 6: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat(settings): add shared field add/remove helpers"
```

---

### Task 4: The Configure available sensors window

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Consumes: `field_checked`, `add_field_to_row`, `remove_field_everywhere` from Task 3.
- Consumes: `monitor_lhm::Reader` through the existing `picker_list` / `PickerSensor`.
- Produces: `fn custom_label(text: &str) -> String`
- Produces: `Manager::sensor_window_ui(&mut self, ctx: &egui::Context)`
- Removes: the inline Browse sensors frame, the paste box, the old custom-sensor list, and the `custom_id_input` Manager field.
- State is the existing `picker_*` fields: `picker_open`, `picker_sensors`, `picker_filter`, `picker_target`, `picker_busy`, `picker_tx`, `picker_rx`.

- [ ] **Step 1: Add the label helper**

Next to `add_picked_sensor` in `src/bin/quantdock.rs`:

```rust
fn custom_label(text: &str) -> String {
    text.to_uppercase().chars().take(12).collect()
}
```

Use it inside `add_picked_sensor`:

```rust
    let label = custom_label(&sensor.text);
```

- [ ] **Step 2: Add the button and remove the old picker UI**

In `settings_ui`, replace the existing Fields heading block (the separator, heading, the
weak hint and `changed |= self.fields_ui(ui);`) with:

```rust
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
```

Then delete the entire old `Custom sensors` section: from its
`ui.separator(); ui.heading("Custom sensors");` through the
`for (id, mut label) in custom { ... }` loop. That removes the paste box, the
**Browse sensors** frame and the old label/Remove list. Task 5 adds the full custom field
table after `fields_ui`; do not ship between the two tasks.

In the existing `Return` button handler, add `self.picker_open = false;` so leaving
Settings does not leave the sensor window floating over the model list.

- [ ] **Step 3: Add the sensor window**

Add this method inside `impl Manager`, after `start_picker_fetch`:

```rust
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
```

- [ ] **Step 4: Show the window**

At the end of `impl eframe::App for Manager::ui`, after the `CentralPanel` block, add:

```rust
        self.sensor_window_ui(&ctx);
```

Task 6 adds `self.profile_dialog_ui(&ctx);` on the next line.

Leave `custom_id_input` in the `Manager` struct and `Manager::new`; Task 5 reuses it for
the **Add by SensorId** fallback under the custom table.

- [ ] **Step 5: Run the tests and the app**

Run: `cargo test --all-targets`

Expected: PASS.

Run: `cargo build --release`

Expected: PASS.

Run `target\release\quantdock.exe`, then Settings -> **Configure available sensors…**. Verify: the window opens at about 900×600, search filters, checking a built-in adds it to the target row, unchecking removes it from every row, and an LHM sensor gets a checkbox. With LHM stopped, stored custom sensors appear under **Stored / missing**.

- [ ] **Step 6: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat(settings): replace the inline picker with a sensor manager window"
```

---

### Task 5: Custom sensor fields table

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Consumes: `forget_custom_field` and `add_field_to_row` from Task 3.
- Consumes: `custom_label` from Task 4.
- Produces: `Manager::custom_fields_ui(&mut self, ui: &mut egui::Ui) -> bool`
- Produces: a module-level `fn style_name(style: Style) -> &'static str` shared by both field tables.

- [ ] **Step 1: Hoist `style_name`**

Remove the nested `fn style_name` from `fields_ui`, drop `Style` from that function's local
`use` list, add `Style` to the top-level import:

```rust
use quantdock::monitor_config::{self, Config, Corner, Style, Watcher};
```

and add this function next to `dot` in `src/bin/quantdock.rs`:

```rust
fn style_name(s: Style) -> &'static str {
    match s {
        Style::Text => "text",
        Style::Bar => "bar",
        Style::Graph => "graph",
        Style::GraphText => "graph+text",
    }
}
```

- [ ] **Step 2: Wire the table into Settings**

In `settings_ui`, immediately after `changed |= self.fields_ui(ui);`, add:

```rust
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
```

This keeps the documented paste fallback: it stores the id even with no rows, appears under
**Stored / missing** in the sensor window, and uses the global duplicate check when it
appends to the last row.

- [ ] **Step 3: Add the custom table**

Add this method to `impl Manager`, after `fields_ui`:

```rust
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
                style: self
                    .cfg
                    .monitor
                    .field_style
                    .get(&id)
                    .copied()
                    .unwrap_or_default(),
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
                        state.style = Style::Text;
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
            if &state.label != &before.0 {
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
```

`fields_ui` keeps its own `use` list, minus the nested `style_name`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --all-targets`

Expected: PASS. This task has no new pure test; the destructive path is covered by `forget_removes_rows_custom_style_and_threshold` and the resolver by the Task 2 tests.

- [ ] **Step 5: Manual check in the app**

Run `target\release\quantdock.exe`. In Settings -> **Custom sensor fields**:

1. Change a label; the preview updates.
2. Set a `/load/` sensor to `bar`; the preview draws a bar with `%` text.
3. Set a non-load sensor to a style; it stays `text` and the control is not shown.
4. Set warn/crit; the value colour changes in the preview.
5. Press **Forget**; the sensor leaves every row and its label/style/thresholds are gone.
6. Type an unreported id into **Add by SensorId**; it is stored and appears under **Stored /
   missing** in the sensor window.

- [ ] **Step 6: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat(settings): configure stored custom sensor fields"
```

---

### Task 6: Ten profile slots and the confirmation modal

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Consumes: `Config::profile`, `snapshot`, `set_profile`, `apply_profile`, `matches_a_profile` from Task 1.
- Produces: `#[derive(Debug, Clone, Copy, PartialEq, Eq)] enum ProfileDialog { Load(usize), Overwrite(usize) }`
- Produces: `Manager.profile_slot: usize`, `Manager.profile_dialog: Option<ProfileDialog>`
- Produces: `Manager::profiles_ui(&mut self, ui: &mut egui::Ui) -> bool`
- Produces: `Manager::profile_dialog_ui(&mut self, ctx: &egui::Context)`

- [ ] **Step 1: Add the dialog type and Manager state**

Next to `enum Tab`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProfileDialog {
    Load(usize),
    Overwrite(usize),
}
```

Add to `Manager`:

```rust
    profile_slot: usize,
    profile_dialog: Option<ProfileDialog>,
```

Initialize in `Manager::new`:

```rust
            profile_slot: 0,
            profile_dialog: None,
```

- [ ] **Step 2: Add the profile section**

Add this method to `impl Manager`, after `custom_fields_ui`:

```rust
    fn profiles_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        for row in 0..2 {
            ui.horizontal(|ui| {
                for col in 0..5 {
                    let slot = row * 5 + col;
                    let filled = self.cfg.profile(slot).is_some();
                    let text = if filled {
                        egui::RichText::new((slot + 1).to_string())
                            .color(egui::Color32::from_rgb(120, 190, 255))
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
            "Slot {} — {}",
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
```

- [ ] **Step 3: Wire the section between Rows and Fields**

In `settings_ui`, after `changed |= self.rows_ui(ui);` and before the Fields heading, add:

```rust
                ui.separator();
                ui.heading("Profiles");
                ui.weak("Save the whole overlay setup to a slot, or load one. Sensor settings stay as they are.");
                changed |= self.profiles_ui(ui);
```

In the existing `Return` button handler, add `self.profile_dialog = None;` next to the
reload so a pending confirmation does not survive leaving Settings.

- [ ] **Step 4: Add the confirmation window**

Add this method after `profiles_ui`:

```rust
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
```

- [ ] **Step 5: Show the dialog and add the App call**

Make sure the end of `Manager::ui` calls both windows:

```rust
        self.sensor_window_ui(&ctx);
        self.profile_dialog_ui(&ctx);
```

- [ ] **Step 6: Run the tests and check the flows**

Run: `cargo test --all-targets`

Expected: PASS.

Run `target\release\quantdock.exe` and check:

1. An empty slot saves without a dialog; the button becomes a filled number.
2. Saving into a filled slot shows **Overwrite / Cancel**; Cancel changes nothing.
3. Editing the layout then pressing **Load** on another filled slot shows only **Load anyway / Cancel**.
4. Load anyway restores rows, thresholds and colours; the hardware-sensor path and interval are unchanged.
5. **Return** discards unsaved profile saves; a saved profile survives a restart after **Save**.
6. `%APPDATA%\QuantDock\settings.toml` contains `[profiles.1]` after saving.

- [ ] **Step 7: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat(settings): add ten overlay profiles with load confirmation"
```

---

### Task 7: Left-align audit and documentation

**Files:**
- Modify: `src/bin/quantdock.rs` (only if the audit finds body text)
- Modify: `README.md`
- Modify: `INSTALL.md`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: the final UI from Tasks 4–6.

- [ ] **Step 1: Run the alignment audit**

Run:

```powershell
rg -n "Layout::centered|vertical_centered|horizontal_centered|Align2::CENTER_CENTER" src
```

Expected matches only:

- `src/bin/monitor.rs` — the overlay close glyph.
- `src/monitor_panel.rs` — the value drawn inside a bar.

Anything else is body text or a label and must move to the default left layout. `Layout::right_to_left` is allowed only for action clusters (Load, Save/Return, Monitor/Refresh, Add/Remove, profile actions). The sensor window and profile dialogs must not use centred layouts.

- [ ] **Step 2: Verify the narrow window by hand**

Run the release app, resize the manager to its 520 px minimum, and check:

- Model-card titles, alias/args labels and action buttons do not overlap.
- Settings headings, prose, the Rows chips, the Fields tables, the custom table, the profile grid and the preview all start at the left edge.
- The sensor window and both profile dialogs keep their text at the left edge.

If something is centred, fix that call site in `src/bin/quantdock.rs`; do not add a global style override.

- [ ] **Step 3: Rewrite the README custom-sensor section**

In the feature list, replace the Settings bullet with:

```markdown
- **Settings** tab: overlay width, interval, opacity, font size, rows, profiles, fields,
  colours and thresholds.
- Body text, tables and the preview stay left-aligned at any window width; only action
  buttons sit on the right.
```

Replace the current `### Custom sensors` section with the final block in this step (a level
two heading, not a Tests subsection), and change `### Saving settings` to
`## Saving settings` so it is no longer nested under Tests. Also replace the coverage
paragraph with:

```markdown
Covers the model config round-trip (comments preserved), the generated llama-server command
line, model discovery, the overlay config (defaults, migration, file safety), sampling
helpers, sensor parsing (including escaped `\u00B0` values), the sampler's LHM cache,
panel formatting, custom-field resolution, profile snapshots, thresholds, geometry, the
bounded server-log tail, and the `gpu_mem_temp` / autostart migrations.
```

Replacement section:

````markdown
## Sensors, custom fields and profiles

Under Settings -> **Fields**, press **Configure available sensors…**. The window lists
every built-in field and every sensor LibreHardwareMonitor reports, with a search box and
a target row. Tick a field to add it to that row; untick it to remove it from every row.
Unticking keeps the field's label, style and thresholds, so ticking it again restores
them. A stored custom sensor the machine no longer reports stays listed under **Stored /
missing** and shows `--` in the overlay; it is never remapped.

The **Custom sensor fields** table below the built-in table edits each stored custom
sensor: label, style (`text`, or `bar` for `/load/` sensors), warn, crit and direction.
**Forget** removes it from every row and deletes its stored label, style and thresholds.
For a SensorId LibreHardwareMonitor is not currently reporting, use **Add by SensorId**
below the table; it is stored and shows `--` until the sensor appears.

**Profiles** saves the whole overlay setup into one of ten slots: rows, custom sensors,
field styles, thresholds, colours, width, interval, opacity, font, position, title and
the always-on-top/close-button flags. Loading a profile when the current layout does not
match any saved slot asks for confirmation. The dialog offers only **Load anyway** and
**Cancel**; Load anyway also discards unsaved overlay edits in the draft. The
LibreHardwareMonitor path, enable flag, polling interval and autostart are machine
settings and are not part of a profile.

Hand edits still work. Custom labels live in `settings.toml`:

```toml
[monitor.custom]
"lhm:/nvidiagpu/0/clock/1" = "GPU CLK"
```

Saved profiles use numbered tables:

```toml
[profiles.1.monitor]
width = 420.0

[[profiles.1.monitor.rows]]
fields = ["gpu", "vram"]

[profiles.1.thresholds.gpu_temp]
warn = 70.0
crit = 82.0
direction = "above"
```
````

- [ ] **Step 4: Add the INSTALL setup note**

In `INSTALL.md`, add this section immediately before `## 5. Start and verify`:

```markdown
## Overlay setup (optional)

After the first launch, Settings -> **Fields** -> **Configure available sensors…** lists
every built-in field and every live LibreHardwareMonitor sensor. Tick the ones you want
and pick the target row. The **Custom sensor fields** table edits their labels, bars and
warning thresholds. **Profiles** stores the whole overlay setup in one of ten slots;
loading a profile asks before replacing an unsaved layout. Sensor paths and intervals
stay machine settings and are not stored in a profile. The manager's body text stays
left-aligned at any window width.
```

- [ ] **Step 5: Update AGENTS.md**

Make these exact changes:

1. Replace "Start here" item 3 with:

   ```markdown
   3. `docs/superpowers/specs/2026-11-15-sensor-window-and-profiles-design.md` - the current
      feature spec: the sensor manager window, custom field config, ten profiles, and left
      alignment. The log-console and memory specs remain as background.
   ```

   Keep the existing paragraph that marks the monitor-lifecycle spec as partly superseded.
2. The code map row for `src/bin/quantdock.rs` says
   `Manager UI: model cards, Settings draft, sensor manager window, profiles, field tables, themes, preview, LHM setup, log console.`
3. The code map row for `src/monitor_config.rs` adds `profiles`.
4. The code map row for `src/monitor_panel.rs` says
   `Overlay renderer: FieldView resolver, rows, text/bar/graph styles, thresholds, auto width.`
5. Under "Current state", add:
   `- Sensor manager window (all built-ins + LHM), custom field config table, ten profiles with Load anyway/Cancel.`
6. In "Commands", replace the test count with the count printed by
   `cargo test --all-targets` (expected 86: 73 lib + 13 bin, 4 ignored). Do not copy the
   old 71/73 line.
7. Add an "Open work" item:

   ```markdown
   3. **Sensor-manager crash.** The old inline picker is gone, but the crash was never
      reproduced. Check `%APPDATA%\QuantDock\crash.log` and the new window before claiming
      it fixed.
   ```

- [ ] **Step 6: Commit the docs and any alignment fix**

```bash
git add README.md INSTALL.md AGENTS.md src/bin/quantdock.rs
git commit -m "docs: document the sensor window, profiles and left alignment"
```

---

### Task 8: Full verification

**Files:** none.

- [ ] **Step 1: Run the full suite and release build**

```bash
cargo test --all-targets
cargo build --release
```

Expected: all tests pass; both `target\release\quantdock.exe` and `target\release\monitor.exe` exist.

- [ ] **Step 2: Exercise the new UI**

Run `target\release\quantdock.exe` and check each line:

- [ ] The sensor window opens, searches, and closes with the title-bar X.
- [ ] Checking a built-in adds it only once; unchecking removes it from all rows; re-checking restores its style and thresholds.
- [ ] Checking the same LHM sensor twice does not duplicate it.
- [ ] With no rows, the sensor window says `Add a row first` and does not add.
- [ ] A `/load/` custom sensor can be a bar; a temperature/power sensor cannot.
- [ ] A missing custom sensor shows `(missing)`, `--`, and no zero bar.
- [ ] Profiles: empty save, overwrite confirmation, dirty load confirmation, Cancel, Load anyway.
- [ ] Loading a profile leaves `sensors.path`, `enabled`, `interval_ms` and `autostart` unchanged.
- [ ] Save, restart the app, and confirm the profile and layout survive.
- [ ] At 520 px window width, no body text is centred; action clusters are the only right-aligned groups.

- [ ] **Step 3: Check the crash evidence**

Before closing the app, exercise the sensor window with search, check, uncheck, target-row changes, empty rows and LHM stopped. Then:

```powershell
if (Test-Path "$env:APPDATA\QuantDock\crash.log") {
    Get-Content "$env:APPDATA\QuantDock\crash.log" -Tail 40
} else {
    "no crash.log"
}
```

If a crash appears, report the action and the log; do not claim the crash fixed. If none appears, report the crash as **not reproduced**, not as fixed.

- [ ] **Step 4: Do not claim the memory waiver changed**

This work adds no sampler or polling path. The NVIDIA driver-floor waiver stays as documented. No working-set trimming.

---

## Review Focus

- A built-in uncheck removes every occurrence but leaves its style/thresholds, so re-checking restores them (Task 3).
- A custom sensor is stored once even when checked from another row (Tasks 3–4).
- `FieldView` covers every draw and width path; no custom field is measured as text when it is a bar (Task 2).
- Profiles round-trip through `[profiles.N]` and never carry `sensors` (Task 1).
- Load anyway is lossless on Cancel; overwrite is never silent (Task 6).
- At 520 px, body text starts left; only the close glyph and bar values are centred (Task 7).
- The old inline picker path is gone; `%APPDATA%\QuantDock\crash.log` remains the evidence path, and the crash is not called fixed without a reproduction (Task 8).
