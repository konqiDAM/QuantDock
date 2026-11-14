# Configurable Monitor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the fixed five-line overlay into a configurable panel whose rows, metrics, styles, thresholds and colours are edited in the main QuantDock window.

**Architecture:** A `monitor_config` module owns the TOML model and its file; `monitor_sample` owns one background sampling thread and per-field history; `monitor_panel` renders rows from a config plus a sample and is shared verbatim by the overlay and the settings preview. The overlay becomes config + sampler + render, and the manager gains the editor.

**Tech Stack:** Rust 1.100 (edition 2024), eframe/egui 0.37 with the glow renderer, `serde` + `toml_edit` (already a dependency; do not add the `toml` crate), `sysinfo` 0.40, `windows` 0.62, `nvidia-smi` via `std::process`.

**Spec:** `docs/superpowers/specs/2026-11-14-monitor-config-design.md`

## Global Constraints

- Windows only. Overlay and manager are GUI-subsystem binaries with no console window.
- All child processes spawn with `CREATE_NO_WINDOW` via the existing `quantdock::hidden` helper.
- Never block the UI thread on `nvidia-smi`, HTTP, or a filesystem stat that can take a lock.
- All window sizing and placement goes through the HWND in physical pixels, never through egui logical-point viewport commands.
- Settings live at `%APPDATA%\QuantDock\settings.toml`; window position at `%APPDATA%\QuantDock\window.toml`.
- A malformed settings file is never overwritten. The manager shows the parse error and offers reset.
- `interval_ms` is clamped to 250..=10000. `graph_samples` is clamped to 1..=600.
- One overlay instance, guarded by the named mutex `Local\QuantDockMonitor`.
- Existing behaviour to preserve: model discovery, load/unload, comment-preserving model `.toml` edits, the three aliases, single-instance overlay.

## Review Focus

- **A field id in `rows` that does not exist** (typo, removed field): must render as `--` or be skipped, never panic, and must survive a save.
- **`width` too small for one field**: must wrap to another line, never clip or grow the window past its configured width.
- **`graph_samples` of 0 or 1**: sparkline must render a flat line, never divide by zero or panic.
- **Settings file deleted, emptied, or malformed while running**: the overlay keeps its last good config; the manager refuses to save over it and offers reset.
- **150 % display scaling**: the window must fit inside the monitor work area and the bar must stay inside the right margin at 0 % and 100 %.

---

## File Structure

```
src/lib.rs                 add three modules
src/monitor_config.rs      Config, Row, FieldDef, Style, Threshold, Theme, load/save, defaults,
                           hex colours, watcher, window position
src/monitor_sample.rs      Value, Sample, History, sampler thread, source parsing
src/monitor_panel.rs       format_value, color_for, bar_rect, spark_points, render
src/bin/monitor.rs         rewritten: config + sampler + render + drag-position save
src/bin/quantdock.rs       settings section, row builder, field table, theme, preview, button
```

Each module keeps its unit tests inline in a `#[cfg(test)] mod tests` block, matching `src/model.rs`.

---

### Task 1: Config model, defaults, and file I/O

**Files:**
- Create: `src/monitor_config.rs`
- Modify: `src/lib.rs`
- Test: inline tests in `src/monitor_config.rs`

**Interfaces:**
- Produces:
  - `pub struct Config { pub monitor: MonitorSettings, pub thresholds: BTreeMap<String, Threshold>, pub theme: Theme }`
  - `pub struct MonitorSettings { pub width: f32, pub interval_ms: u64, pub background_alpha: u8, pub corner_radius: f32, pub font_size: f32, pub show_labels: bool, pub always_on_top: bool, pub show_close: bool, pub position: Corner, pub margin: f32, pub graph_samples: usize, pub rows: Vec<Row>, pub field_style: BTreeMap<String, Style> }`
  - `pub struct Row { pub fields: Vec<String> }`
  - `pub struct Threshold { pub warn: Option<f64>, pub crit: Option<f64>, pub direction: Direction }`
  - `pub enum Direction { Above, Below }`, `pub enum Style { Text, Bar, Graph, GraphText }`, `pub enum Corner { TopRight, TopLeft, BottomRight, BottomLeft }`
  - `pub struct Theme { pub text: Color32, pub label: Color32, pub accent: Color32, pub warn: Color32, pub crit: Color32, pub background: Color32 }` (`#[derive(Default)]`, with the values below)
  - `pub struct FieldDef { pub id: &'static str, pub label: &'static str, pub kind: FieldKind }`, `pub enum FieldKind { Text, Percent, Number, Gb, Celsius, Watts, Mhz, Count }`
  - `pub const FIELDS: &[FieldDef]`, `pub fn field_def(id: &str) -> Option<&'static FieldDef>`
  - `impl Config { pub fn defaults() -> Self; pub fn load(path: &Path) -> Result<Self, String>; pub fn save(&self, path: &Path) -> std::io::Result<()>; pub fn validate(&self) -> Vec<String> }`
  - `pub fn settings_path() -> PathBuf`, `pub fn data_dir() -> PathBuf`
  - `pub fn parse_hex(s: &str) -> Option<Color32>`, `pub fn to_hex(c: Color32) -> String`
- Consumes: `egui::Color32` (already a dependency), `serde`.

- [ ] **Step 1: Write the failing tests**

Cover, in `src/monitor_config.rs`:

```rust
#[test]
fn defaults_match_the_spec() {
    let c = Config::defaults();
    let ids: Vec<Vec<&str>> = c.monitor.rows.iter()
        .map(|r| r.fields.iter().map(String::as_str).collect()).collect();
    assert_eq!(ids, vec![
        vec!["model"],
        vec!["gpu", "vram"],
        vec!["cpu", "cpu_peak", "cpu_hot"],
        vec!["temp", "fan", "power"],
    ]);
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
    assert!(c.monitor.rows[1].fields.contains(&"gpu_temp_magic".to_string()));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test monitor_config`
Expected: FAIL — `monitor_config` module not found.

- [ ] **Step 3: Implement the module**

Add `pub mod monitor_config;` to `src/lib.rs`. Implement the types above with serde derives. Use `#[serde(with = "hex_color")]` on each `Theme` field and `#[serde(default)]` on optionals so a partial file loads.

`Config::defaults()` returns exactly the generated config in the spec:
width 320, interval 1000, alpha 190, radius 12, font 13, labels on, always-on-top on, close on, `TopRight`, margin 16, graph_samples 60, the four rows above, `gpu = Bar` / `cpu = GraphText`, thresholds `temp 70/82`, `gpu 90/98`, `cpu_peak 80/95`, `vram_pct 90/97`, `disk_free 100/40 Below`, and the theme hex values `#d6d6d6 #8a8a8a #78beff #e0b050 #e07878 #000000`.

`Config::load` reads the file, parses, and clamps `interval_ms` and `graph_samples`. `save` serializes, writes to a temp file in the same directory, then renames, so a crash cannot truncate the real file.

`validate` returns one warning per field id not present in `FIELDS`. Unknown ids are kept so a save does not silently delete a user's row while the app is open.

`data_dir()` returns `%APPDATA%\QuantDock`, created if missing; `settings_path()` is `data_dir().join("settings.toml")`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test monitor_config`
Expected: PASS, five tests.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/monitor_config.rs
git commit -m "feat: monitor config model with defaults and safe file I/O"
```

---

### Task 2: Config watcher and window position file

**Files:**
- Modify: `src/monitor_config.rs`
- Test: inline tests

**Interfaces:**
- Produces:
  - `pub struct Watcher { pub path: PathBuf, last: Option<SystemTime> }` with `pub fn new(path: PathBuf) -> Self` and `pub fn changed(&mut self) -> bool` (compares mtime, returns true once per change).
  - `pub fn window_path() -> PathBuf`
  - `pub fn load_window_pos() -> Option<egui::Pos2>`, `pub fn save_window_pos(pos: egui::Pos2) -> std::io::Result<()>`, `pub fn clear_window_pos()`
- Consumes: Task 1's `data_dir`.

- [ ] **Step 1: Write the failing tests**

```rust
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
fn window_pos_round_trips() {
    let pos = egui::pos2(1234.0, 56.0);
    save_window_pos(pos).unwrap();
    assert_eq!(load_window_pos(), Some(pos));
    clear_window_pos();
    assert_eq!(load_window_pos(), None);
}
```

`window_pos_round_trips` touches the real `%APPDATA%`; mark it `#[ignore]` and run it explicitly in Step 4 so it cannot clobber a user's saved position during normal test runs.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test monitor_config::tests::watcher`
Expected: FAIL — `Watcher` not found.

- [ ] **Step 3: Implement**

`Watcher::changed` stats the path; if the mtime differs from `last`, store it and return true. A stat error resets `last` to `None` and returns false. The caller throttles to once a second; the watcher itself does not sleep.

`save_window_pos` writes `x = <f32>` / `y = <f32>` to `window_path()`; `load_window_pos` parses it; `clear_window_pos` ignores a missing file.

- [ ] **Step 4: Run the tests**

Run: `cargo test monitor_config` and `cargo test -- --ignored window_pos`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/monitor_config.rs
git commit -m "feat: config watcher and window position persistence"
```

---

### Task 3: Sampling thread with history

**Files:**
- Create: `src/monitor_sample.rs`
- Modify: `src/lib.rs`, `src/gpu.rs` (extend the nvidia-smi query)
- Test: inline tests

**Interfaces:**
- Produces:
  - `pub enum Value { Num(f64), Text(String), UsedTotal(f64, f64) }` — `UsedTotal` carries used and total for `vram` and `swap`
  - `pub struct Sample { pub fields: BTreeMap<&'static str, Value>, pub history: BTreeMap<&'static str, Vec<f64>> }` (`#[derive(Default)]`)
  - `pub struct History { pub cap: usize, pub items: VecDeque<f64> }` with `pub fn new(cap: usize) -> Self`, `push`, `as_slice`
  - `pub fn start_sampler(cfg: Arc<RwLock<Config>>) -> std::sync::mpsc::Receiver<Sample>`
  - `pub fn needs_gpu(cfg: &Config) -> bool`
  - `pub fn parse_nvidia_smi(line: &str) -> Option<GpuSample>`
  - `pub fn aggregate_cores(cores: &[f32], hot_at: f32) -> Cores`
  - `pub struct Cores { pub total: f32, pub peak: f32, pub hot: usize }`
  - `pub struct GpuSample { pub util: f32, pub mem_util: Option<f32>, pub mem_used_mb: f32, pub mem_total_mb: f32, pub temp: Option<f32>, pub fan: Option<f32>, pub power: Option<f32>, pub power_limit: Option<f32>, pub clock: Option<f32>, pub pstate: String }`
- Consumes: `crate::hidden`, `crate::gpu`, `crate::server`, `crate::model`, `monitor_config::{Config, FieldDef}`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn parses_full_nvidia_smi_line() {
    let g = parse_nvidia_smi("5, 1, 3460, 24576, 52, 61, 112.15, 420.00, 225, P0").unwrap();
    assert_eq!(g.util, 5.0);
    assert_eq!(g.mem_used_mb, 3460.0);
    assert_eq!(g.temp, 52.0);
    assert_eq!(g.pstate, "P0");
}

#[test]
fn parses_na_fields_as_none() {
    let g = parse_nvidia_smi("0, 0, 100, 24576, 40, [N/A], [N/A], 420.00, 210, P8").unwrap();
    assert_eq!(g.fan, None);
    assert_eq!(g.power, None);
}

#[test]
fn malformed_line_is_none() {
    assert!(parse_nvidia_smi("garbage").is_none());
}

#[test]
fn aggregates_cores() {
    let c = aggregate_cores(&[10.0, 95.0, 82.0, 3.0], 80.0);
    assert_eq!(c.peak, 95.0);
    assert_eq!(c.hot, 2);
    assert!((c.total - 47.5).abs() < 0.01);
}

#[test]
fn history_respects_capacity() {
    let mut h = History::new(3);
    for v in [1.0, 2.0, 3.0, 4.0] { h.push(v); }
    assert_eq!(h.as_slice(), &[2.0, 3.0, 4.0]);
}

#[test]
fn gpu_is_skipped_when_no_gpu_field_is_shown() {
    let mut c = Config::defaults();
    c.monitor.rows = vec![Row { fields: vec!["cpu".into()] }];
    assert!(!needs_gpu(&c));
    c.monitor.rows[0].fields.push("gpu".into());
    assert!(needs_gpu(&c));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test monitor_sample`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

`gpu.rs` gains `query_all() -> Option<GpuSample>` running one `nvidia-smi` call with `--query-gpu=utilization.gpu,utilization.memory,memory.used,memory.total,temperature.gpu,fan.speed,power.draw,power.limit,clocks.sm,pstate`. The existing `sample()` stays for the current overlay until Task 6 replaces it.

`parse_nvidia_smi` splits on commas, trims, and maps `[N/A]` to `None`. `aggregate_cores` returns total (mean), peak (max) and hot (count `>= warn`), taking the warn value as a parameter.

`start_sampler` spawns one thread:

```
loop {
  let cfg = cfg.read().clone();
  build a Sample by calling only the sources the visible fields need;
  push each numeric value into its History in a HashMap<&'static str, History>;
  send the Sample with histories cloned in;
  sleep(cfg.monitor.interval_ms);
  exit when the receiver is dropped
}
```

Sources: `nvidia-smi` only when `needs_gpu`; sysinfo CPU/memory/swap always (cheap); disks and process RSS only if `disk_free` or `llama_ram`/`uptime` appear; `/v1/models` + `/props` only if `model`/`alias`/`ctx` appear. `cpu_hot` uses `cfg.thresholds["cpu_peak"].warn` or 80.

Text fields map to `Value::Text`; everything else to `Value::Num`. `ctx` formats as `262K` when >= 1000.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test monitor_sample`
Expected: PASS, six tests.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/gpu.rs src/monitor_sample.rs
git commit -m "feat: field-gated sampler with per-field history"
```

---

### Task 4: Renderer primitives and text rows

**Files:**
- Create: `src/monitor_panel.rs`
- Modify: `src/lib.rs`
- Test: inline tests

**Interfaces:**
- Produces:
  - `pub fn visible_fields(row: &Row) -> Vec<&'static FieldDef>` — drops unknown ids
  - `pub fn format_value(def: &FieldDef, v: Option<&Value>, sample: &Sample) -> String`
  - `pub fn color_for(t: Option<&Threshold>, value: f64, theme: &Theme) -> Color32`
  - `pub fn bar_rect(content: egui::Rect, frac: f64) -> egui::Rect`
  - `pub fn render(ui: &mut egui::Ui, cfg: &Config, sample: &Sample) -> egui::Vec2` — returns the used size
- Consumes: Tasks 1 and 3.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn formats_percent_and_bytes() {
    let mut s = Sample::default();
    s.fields.insert("gpu", Value::Num(15.0));
    assert_eq!(format_value(field_def("gpu").unwrap(), s.fields.get("gpu"), &s), "15%");

    s.fields.insert("vram", Value::UsedTotal(3.2 * 1024.0, 24.0 * 1024.0));
    assert_eq!(format_value(field_def("vram").unwrap(), s.fields.get("vram"), &s),
               "3.2/24.0 GB");
}

#[test]
fn missing_value_renders_dashes() {
    let s = Sample::default();
    assert_eq!(format_value(field_def("temp").unwrap(), None, &s), "--");
}

#[test]
fn thresholds_pick_colours() {
    let theme = Theme::default();
    let t = Threshold { warn: Some(70.0), crit: Some(82.0), direction: Direction::Above };
    assert_eq!(color_for(Some(&t), 52.0, &theme), theme.text);
    assert_eq!(color_for(Some(&t), 70.0, &theme), theme.warn);
    assert_eq!(color_for(Some(&t), 82.0, &theme), theme.crit);
    assert_eq!(color_for(None, 999.0, &theme), theme.text);
}

#[test]
fn below_thresholds_invert() {
    let theme = Theme::default();
    let t = Threshold { warn: Some(100.0), crit: Some(40.0), direction: Direction::Below };
    assert_eq!(color_for(Some(&t), 400.0, &theme), theme.text);
    assert_eq!(color_for(Some(&t), 100.0, &theme), theme.warn);
    assert_eq!(color_for(Some(&t), 40.0, &theme), theme.crit);
}

#[test]
fn bar_never_exceeds_its_content_rect() {
    let content = egui::Rect::from_min_size(egui::pos2(10.0, 5.0), egui::vec2(200.0, 18.0));
    assert_eq!(bar_rect(content, 0.0).width(), 0.0);
    assert_eq!(bar_rect(content, 1.0).right(), content.right());
    assert!(bar_rect(content, 2.0).right() <= content.right(), "clamped above 1.0");
    assert!(bar_rect(content, -1.0).width() >= 0.0, "clamped below 0.0");
}

#[test]
fn unknown_fields_are_dropped_not_rendered() {
    let row = Row { fields: vec!["gpu".into(), "not_a_field".into(), "cpu".into()] };
    let visible: Vec<&str> = visible_fields(&row).iter().map(|d| d.id).collect();
    assert_eq!(visible, vec!["gpu", "cpu"]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test monitor_panel`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement**

`format_value` switches on `FieldKind`: `Percent` → `"{v:.0}%"`; `Gb` → one decimal with `/total` appended for `vram` and `swap`; `Celsius` → `"{v:.0} °C"`; `Watts` → `"{v:.0} W"`; `Mhz` → `"{v:.0} MHz"`; `Count` → integer; `Number` → integer below 100, one decimal above; `None` → `"--"`.

`color_for` compares against `crit` first, then `warn`, honouring `direction`; missing thresholds return `theme.text`.

`render` builds the frame: paints the rounded background over `ui.max_rect()` using `theme.background` at `background_alpha` and `corner_radius`, then lays out rows with `ui.horizontal_wrapped` and `item_spacing.x = 12`, all inside `Frame::NONE.inner_margin(12, 10)`. It first sets every text style to `font_size` so the config actually governs type size. Rows resolve through `visible_fields`. Text style draws `<label> <value>` with the label in `theme.label`, the value in `color_for`, and `model`/`alias` values in `theme.accent`; when `show_labels` is false only the value is drawn. Unknown ids are skipped. Return `ui.min_rect().size()` so the caller can resize.

`visible_fields` maps each id through `field_def` and drops `None`, preserving order; empty ids are kept by the config but never reach rendering.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test monitor_panel`
Expected: PASS, six tests.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/monitor_panel.rs
git commit -m "feat: panel renderer with formatting, thresholds and safe bar geometry"
```

---

### Task 5: Bar, graph and graph_text styles

**Files:**
- Modify: `src/monitor_panel.rs`
- Test: inline tests

**Interfaces:**
- Produces:
  - `pub fn spark_points(history: &[f64], max: f64, rect: egui::Rect) -> Vec<egui::Pos2>`
  - `render` gains the three remaining styles
- Consumes: Task 4's `bar_rect`, `color_for`, `format_value`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn spark_starts_at_left_and_ends_at_right() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
    let pts = spark_points(&[0.0, 50.0, 100.0], 100.0, rect);
    assert_eq!(pts.len(), 3);
    assert_eq!(pts[0].x, rect.left());
    assert_eq!(pts[2].x, rect.right());
    assert_eq!(pts[0].y, rect.bottom());
    assert_eq!(pts[2].y, rect.top());
}

#[test]
fn spark_with_one_point_is_flat_not_empty() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
    let pts = spark_points(&[42.0], 100.0, rect);
    assert_eq!(pts.len(), 2, "one sample is drawn as a flat two-point line");
    assert_eq!(pts[0].y, pts[1].y);
}

#[test]
fn spark_with_no_points_is_empty() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
    assert!(spark_points(&[], 100.0, rect).is_empty());
}

#[test]
fn spark_clamps_out_of_range_values() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 20.0));
    let pts = spark_points(&[-50.0, 500.0], 100.0, rect);
    assert!(pts.iter().all(|p| rect.contains(*p)));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test monitor_panel::tests::spark`
Expected: FAIL — `spark_points` not found.

- [ ] **Step 3: Implement**

`Bar`: allocate the field's share of the row, fill `bar_rect` with `color_for` at 60 % alpha, draw the value text centred on top in `theme.text`, all inside the content rect so the right margin is preserved.

`Graph`: allocate `graph_samples` points at the field's width and one line of height, map values against `max` (100 for percent, the field's own running max otherwise), and stroke a 1 px polyline in `color_for` of the current value. `GraphText` draws the same sparkline plus `format_value` to its right, separated by 6 px.

A field with `Style::Text` on a text-only field falls back to `Text` regardless of config.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test monitor_panel`
Expected: PASS, ten tests.

- [ ] **Step 5: Commit**

```bash
git add src/monitor_panel.rs
git commit -m "feat: bar, graph and graph+text field styles"
```

---

### Task 6: Rewire the overlay

**Files:**
- Modify: `src/bin/monitor.rs`
- Test: manual verification (no unit tests; all logic lives in the modules)

**Interfaces:**
- Consumes: `Config::load`, `Watcher`, `start_sampler`, `render`, `load_window_pos`, `save_window_pos`.

- [ ] **Step 1: Replace the hard-coded UI**

`Monitor` holds `cfg: Arc<RwLock<Config>>`, `rx: Receiver<Sample>`, `sample: Sample`, `watcher: Watcher`, `hwnd`. `new(cc)` loads the config (falling back to defaults with a logged error), starts the sampler, positions the window (saved `window.toml` position if present, otherwise the configured corner via the existing `place_top_right` generalised to all four corners), and applies `always_on_top` with `SetWindowPos` `HWND_TOPMOST` / `HWND_NOTOPMOST`.

`ui()` drains the receiver, checks `watcher.changed()` at most once a second and re-loads into the `RwLock`, re-applies `always_on_top` when it changed, then calls `render`. `show_close` decides whether the close button and its hit-test exist at all. After layout it compares the returned size with the current window size and sends `ViewportCommand::InnerSize` when they differ by more than 1 px.

On `drag_stopped` (the drag response's `drag_stopped()`), read the window's physical rect and write `save_window_pos`. Keep `StartDrag`, right-click close, and the close button.

- [ ] **Step 2: Fix the ID-clash marker**

Wrap the close button in `ui.push_id("monitor_close", |ui| ui.put(rect, ...))` so exactly one widget owns the id. Verify with the window focused and while dragging that no red marker appears.

- [ ] **Step 3: Verify by hand**

Run: `cargo run --release --bin monitor`
Expected: the overlay matches the default config (model row, GPU/VRAM, CPU/peak/hot, temp/fan/power), `x` closes it, dragging is smooth, restarting restores the dragged position, and editing `%APPDATA%\QuantDock\settings.toml` (change `width` to 160) reflows the panel within a second without moving the window.

- [ ] **Step 4: Commit**

```bash
git add src/bin/monitor.rs
git commit -m "feat: overlay renders from config and remembers its position"
```

---

### Task 7: Settings scaffold, layout controls and reset

**Files:**
- Modify: `src/bin/quantdock.rs`
- Test: manual; persistence covered by Task 1/2 unit tests

**Interfaces:**
- Consumes: `Config::load/save`, `settings_path`, `data_dir`, `clear_window_pos`.
- Produces: `enum Tab { Models, Settings }` and `fn settings_ui(&mut self, ui: &mut egui::Ui)` used by later tasks.

- [ ] **Step 1: Add the tab switch and load the config**

`Manager` gains `tab: Tab`, `cfg: Config`, `cfg_err: Option<String>`. On startup, `Config::load(settings_path())`; on error store the message and show a banner with a **Reset to defaults** button (which writes defaults and clears the error). A malformed file disables all other settings controls.

- [ ] **Step 2: Build the layout controls**

A grid of: width (0 = auto, drag value), interval (250..10000, step 50), background alpha (0..255 slider), corner radius, font size, margin, graph samples (1..600), and checkboxes for show labels / always on top / show close, plus a corner dropdown and a **Reset position** button calling `clear_window_pos()`.

- [ ] **Step 3: Persist on change**

Keep `dirty_at: Option<Instant>`; when any control changes, set it. In `ui()`, if `dirty_at` is older than 300 ms, `cfg.save(&settings_path())` and clear it. Errors go to the existing status message line.

- [ ] **Step 4: Verify by hand**

Run: `cargo run --release --bin quantdock`, switch to Settings, change the width and alpha, and confirm the file on disk changes and the overlay follows within a second. Hand-edit the file while the manager is open and confirm the UI reloads instead of overwriting it.

- [ ] **Step 5: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: settings tab with layout controls and debounced persistence"
```

---

### Task 8: Row builder

**Files:**
- Modify: `src/bin/quantdock.rs`
- Test: manual

**Interfaces:**
- Consumes: `FIELDS`, `field_def`, `Row`.
- Produces: nothing new for other tasks.

- [ ] **Step 1: Build the rows panel**

For each row: its chips in order, each with the field label, an `x`, and `‹` / `›` buttons that swap with the neighbour. Row-level controls: `+ field` (a menu listing `FIELDS` not already in that row), delete row, move up, move down. Below the list: `+ row`.

An empty row renders as a spacer, labelled `(spacer)`.

- [ ] **Step 2: Verify by hand**

Run the manager, build a square layout (`width` 150, one field per row) and a wide one (four fields in one row), confirm the preview follows and the overlay matches, and confirm deleting a row and reopening the app preserves the change.

- [ ] **Step 3: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: row builder for overlay layout"
```

---

### Task 9: Field table and theme

**Files:**
- Modify: `src/bin/quantdock.rs`
- Test: manual; colour parsing covered in Task 1

**Interfaces:**
- Consumes: `FieldDef`, `Style`, `Threshold`, `Theme`, `parse_hex`, `to_hex`.

- [ ] **Step 1: Build the field table**

One row per field id appearing in any row: label, a style dropdown (`text`/`bar`/`graph`/`graph_text`, or only `text` for `FieldKind::Text`), warn and crit drag values (empty = none), and an above/below dropdown. Editing writes to `cfg.thresholds[id]` and `cfg.monitor.field_style[id]`.

- [ ] **Step 2: Build the theme section**

Six swatches: text, label, accent, warn, crit, background. Each opens `egui::color_picker::color_edit_button_srgba`; store back through `to_hex`.

- [ ] **Step 3: Verify by hand**

Set `temp` warn to 40 and confirm the value turns amber in the preview; set `gpu` to `graph` and confirm a sparkline appears in the overlay after a few seconds; change `crit` to green and confirm both update.

- [ ] **Step 4: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: field styles, thresholds and theme editors"
```

---

### Task 10: Settings preview

**Files:**
- Modify: `src/bin/quantdock.rs`
- Test: manual

**Interfaces:**
- Consumes: `monitor_panel::render`.

- [ ] **Step 1: Add the preview**

Below the controls, a framed box rendering `render()` with the current config and a `Sample` built once:

```
model  "Qwen3.8-27B-R3-mix"
alias  "local"
gpu 15, vram 3277/24576 MB, temp 52, fan 61, power 112
cpu 34, cpu_peak 92, cpu_hot 3, ram 12.4/64 GB, disk_free 431 GB
```

with `temp` at 52 (so the default 70 warn does not fire) and `cpu_peak` at 92 (so the default 80 warn shows amber). Give graph-styled fields a synthetic history of `graph_samples` values along a smooth curve.

- [ ] **Step 2: Verify by hand**

Drag the width slider from 0 to 150 and watch the preview reflow and wrap exactly as the overlay does; switch a field to `bar` and `graph_text` and confirm both appear in the preview.

- [ ] **Step 3: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: live settings preview"
```

---

### Task 11: Monitor button states

**Files:**
- Modify: `src/bin/quantdock.rs`
- Test: manual

**Interfaces:**
- Produces: `fn monitor_running() -> bool`, `fn close_monitor()`.

- [ ] **Step 1: Implement state detection and close**

`monitor_running` calls `OpenMutexW(SYNCHRONIZE, false, w!("Local\\QuantDockMonitor"))`; a non-null handle means running, and the handle is closed immediately. It must never call `CreateMutexW`, which would create the mutex and make the overlay think it is already running.

`close_monitor` calls `FindWindowW(None, w!("QuantDock Monitor"))` then `PostMessageW(hwnd, WM_CLOSE, 0, 0)`.

- [ ] **Step 2: Wire the button**

Poll `monitor_running()` at most once a second into `monitor_up: bool`. Render with `egui::Button::new(...).fill(...)`: neutral when down, green when up, red when up and hovered, with the label switching to `Close`. Clicking spawns when down and calls `close_monitor()` when up.

- [ ] **Step 3: Verify by hand**

From the manager: click Monitor (overlay appears, button turns green), hover (red, reads Close), click (overlay exits, button returns to neutral). Launch `monitor.exe` directly and confirm the button reflects it.

- [ ] **Step 4: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: monitor button reflects state and closes the overlay"
```

---

### Task 12: End-to-end pass

**Files:** none; verification only.

- [ ] **Step 1: Run the full suite and a clean build**

Run: `cargo test && cargo build --release`
Expected: all tests pass, no warnings.

- [ ] **Step 2: Exercise the Review Focus list**

For each: unknown field id in `rows`, `width = 100`, `graph_samples = 1`, malformed settings while both apps run, and a 150 % display. Confirm the behaviours in that section, and that the overlay never exceeds the work area.

- [ ] **Step 3: Confirm the two bugs stay fixed**

With the overlay focused and while dragging, no red marker. With `gpu` as a bar at 0 % and 100 %, a visible margin on both sides.

- [ ] **Step 4: Commit any fixes and finish**

```bash
git add -A
git commit -m "fix: issues found in the configurable monitor end-to-end pass"
```
