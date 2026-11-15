# Log Console and Sensor Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a bottom llama-server log console, opt-in LibreHardwareMonitor autostart, restore `gpu_mem_temp` in older configs, and replace paste-only custom sensors with a live picker.

**Architecture:** Config migration and the new `autostart` flag live in `monitor_config`; bounded log reading lives in `server`; all UI state and LHM orchestration live in the manager binary. The picker fetches LHM on a thread and sends a vector back; the probe returns sensor ids for missing markers.

**Tech Stack:** Rust 2024, egui/eframe 0.37, serde/toml_edit, ureq, windows crate. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-15-log-console-sensor-polish-design.md`

## Global Constraints

- No new crates.
- All LibreHardwareMonitor HTTP work stays off the UI thread.
- Unknown field ids are preserved; existing configs keep loading.
- Settings stay draft-based: Save writes, Return discards.
- Log tail is bounded: last 64 KB, at most 200 lines for the console.
- The console is session state; never written to `settings.toml`.
- LHM autostart defaults to false and fires at most once per manager launch.
- Windows-only, as today.

## Review Focus

- A config that already has `gpu_mem_temp` after `gpu_hotspot`: no duplicate, no reorder (Task 1).
- A log file larger than 64 KB read from the middle of a line: the first returned line must be complete, not a suffix (Task 3).
- A missing or unreadable log path: empty vector, no panic (Task 3).
- Autostart with sensors disabled or with no LHM-backed field: never spawn LibreHardwareMonitor (Task 5).
- Adding a sensor whose id is already in the target row: no duplicate (Task 6).

---

### Task 1: Restore `gpu_mem_temp` in older configs

**Files:**
- Modify: `src/monitor_config.rs` (`migrate`, tests)

**Interfaces:**
- Produces: `migrate()` inserts `gpu_mem_temp` immediately after `gpu_hotspot` when the row lacks it.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/monitor_config.rs`:

```rust
#[test]
fn migrates_gpu_mem_temp_after_hotspot() {
    let cfg = Config::parse(
        "[[monitor.rows]]\nfields = [\"gpu_hotspot\", \"gpu_shared\", \"fan\"]\n",
    )
    .unwrap();
    assert_eq!(
        cfg.monitor.rows[0].fields,
        vec!["gpu_hotspot", "gpu_mem_temp", "gpu_shared", "fan"]
    );
}

#[test]
fn does_not_duplicate_gpu_mem_temp() {
    let cfg = Config::parse(
        "[[monitor.rows]]\nfields = [\"gpu_hotspot\", \"gpu_mem_temp\"]\n",
    )
    .unwrap();
    assert_eq!(
        cfg.monitor.rows[0].fields,
        vec!["gpu_hotspot", "gpu_mem_temp"]
    );
}

#[test]
fn leaves_rows_without_hotspot_alone() {
    let cfg = Config::parse("[[monitor.rows]]\nfields = [\"gpu_temp\", \"fan\"]\n").unwrap();
    assert_eq!(cfg.monitor.rows[0].fields, vec!["gpu_temp", "fan"]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --all-targets gpu_mem_temp`

Expected: `migrates_gpu_mem_temp_after_hotspot` and `does_not_duplicate_gpu_mem_temp` fail on the missing field; `leaves_rows_without_hotspot_alone` passes.

- [ ] **Step 3: Implement the sibling insert**

At the end of `migrate()`, after the existing rename loops:

```rust
for row in &mut cfg.monitor.rows {
    let Some(hotspot) = row.fields.iter().position(|id| id == "gpu_hotspot") else {
        continue;
    };
    if row.fields.iter().any(|id| id == "gpu_mem_temp") {
        continue;
    }
    row.fields.insert(hotspot + 1, "gpu_mem_temp".to_owned());
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --all-targets gpu_mem_temp`

Expected: 2 passed. Then run the full suite: `cargo test --all-targets` — expected: all pass, ignored tests skipped.

- [ ] **Step 5: Commit**

```bash
git add src/monitor_config.rs
git commit -m "feat: restore gpu_mem_temp in older configs"
```

---

### Task 2: Add the sensor autostart flag

**Files:**
- Modify: `src/monitor_config.rs` (`SensorsSettings`, its `Default`, tests)

**Interfaces:**
- Produces: `SensorsSettings.autostart: bool`, default `false`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
#[test]
fn sensors_autostart_defaults_to_false() {
    assert!(!Config::defaults().sensors.autostart);
    let cfg = Config::parse("[sensors]\nenabled = true\n").unwrap();
    assert!(!cfg.sensors.autostart);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --all-targets sensors_autostart`

Expected: compile error, `SensorsSettings` has no field `autostart`.

- [ ] **Step 3: Add the field and default**

In `SensorsSettings`:

```rust
/// Start LibreHardwareMonitor with QuantDock.
pub autostart: bool,
```

In `impl Default for SensorsSettings`, add `autostart: false`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --all-targets sensors_autostart`

Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add src/monitor_config.rs
git commit -m "feat: add the sensor autostart setting"
```

---

### Task 3: Bounded log path and tail reader

**Files:**
- Modify: `src/server.rs` (`load`, new `log_path`, new `tail`, new tests)

**Interfaces:**
- Produces: `pub fn log_path(model: &Model) -> PathBuf`
- Produces: `pub fn tail(path: &Path, max_lines: usize) -> Vec<String>`
- `load()` uses `log_path(model)` instead of building the path inline.

- [ ] **Step 1: Write the failing tests**

Add a `#[cfg(test)] mod tests` at the end of `src/server.rs`:

```rust
fn temp_log(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("qd_tail_{name}_{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn tail_returns_the_last_lines() {
    let path = temp_log("last");
    let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&path, body).unwrap();
    let lines = tail(&path, 200);
    assert_eq!(lines.len(), 200);
    assert_eq!(lines[0], "line 300");
    assert_eq!(lines[199], "line 499");
}

#[test]
fn tail_missing_file_is_empty() {
    assert!(tail(&temp_log("missing"), 200).is_empty());
}

#[test]
fn tail_handles_a_short_file() {
    let path = temp_log("short");
    std::fs::write(&path, "one\ntwo\n").unwrap();
    assert_eq!(tail(&path, 200), vec!["one", "two"]);
}

#[test]
fn tail_drops_a_partial_first_line() {
    let path = temp_log("partial");
    let mut body = "x".repeat(70 * 1024);
    body.push('\n');
    for i in 0..300 {
        body.push_str(&format!("line {i}\n"));
    }
    std::fs::write(&path, body).unwrap();
    let lines = tail(&path, 200);
    assert_eq!(lines.len(), 200);
    assert_eq!(lines[0], "line 100");
    assert_eq!(lines[199], "line 299");
    assert!(lines.iter().all(|l| l.starts_with("line ")));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --all-targets tail_`

Expected: compile error, `tail` and `log_path` not found.

- [ ] **Step 3: Implement `log_path` and `tail`**

In `src/server.rs`:

```rust
pub fn log_path(model: &Model) -> PathBuf {
    model.folder.join("llama-server.log")
}

pub fn tail(path: &Path, max_lines: usize) -> Vec<String> {
    // open, take the byte length, seek with SeekFrom::Start(len.saturating_sub(64 * 1024)),
    // read to end, lossy-decode, split lines, and drop the first line when
    // the window did not start at byte 0. Return the last max_lines.
}
```

Change the import to `use std::path::{Path, PathBuf};` (`server.rs` currently
imports only `PathBuf`), and use `std::io::{Read, Seek, SeekFrom}`. On any I/O error
return `Vec::new()`. Use `SeekFrom::Start`, not a negative `SeekFrom::End`, because
a negative seek errors on files shorter than the window. Replace the inline
`model.folder.join("llama-server.log")` in `load()` with `log_path(model)`.

Also rewrite `last_error_line` to use the bounded reader:

```rust
fn last_error_line(path: &Path) -> Option<String> {
    let lines = tail(path, 200);
    lines
        .iter()
        .rev()
        .find(|l| l.to_ascii_lowercase().contains("error"))
        .or_else(|| lines.iter().rev().find(|l| !l.trim().is_empty()))
        .map(|l| l.trim().to_owned())
}
```

This removes the only remaining whole-file read on the failure path.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --all-targets tail_`

Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/server.rs
git commit -m "feat: bounded llama-server log tail"
```

---

### Task 4: Bottom log console

**Files:**
- Modify: `src/bin/quantdock.rs` (`Manager` fields, `Manager::new`, `poll`, bottom bar, new `log_ui`/`refresh_log`, Load button)

**Interfaces:**
- Consumes: `server::log_path`, `server::tail` from Task 3.
- Produces: `fn refresh_log(&mut self)` and `fn log_ui(&mut self, ui: &mut egui::Ui)`.

- [ ] **Step 1: Add console state**

Add to `struct Manager`:

```rust
logs_open: bool,
last_log: Option<(String, PathBuf)>,
log_text: String,
log_at: Instant,
```

Initialize in `Manager::new`: `logs_open: false`, `last_log: None`, `log_text: String::new()`, `log_at: Instant::now() - Duration::from_secs(2)`.

- [ ] **Step 2: Track the log to display**

In the model card `Load` branch, before spawning the load thread:

```rust
self.last_log = Some((m.cfg.name.clone(), server::log_path(&m)));
```

In `poll()`, after `self.status = server::status();`, if `self.last_log.is_none()` and a model is serving, set it from the loaded model's name and `server::log_path`.

- [ ] **Step 3: Add the refresh and panel functions**

```rust
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
```

`log_ui`: header row with `Logs - <name>` or `No model loaded - load a model to see its server log`, the path in weak small text, and `Refresh`, `Open in editor`, `Copy path`, `Close` buttons. `Refresh` sets `log_at` two seconds back. `Copy path` calls `ui.ctx().copy_text(path.display().to_string())`. `Open in editor` runs `quantdock::hidden(std::process::Command::new("cmd").args(["/C", "start", ""]).arg(path)).spawn()`; call `.spawn()`, never `.status()`, because `status()` blocks the UI until the editor exits. Body: if `log_text` is empty show `No log yet for this model.`, otherwise:

```rust
egui::ScrollArea::vertical()
    .stick_to_bottom(true)
    .show(ui, |ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(&self.log_text).monospace())
                .selectable(true),
        );
    });
```

- [ ] **Step 4: Show the panel and the toggle**

After the existing bottom status panel and before `CentralPanel`, add:

```rust
if self.logs_open {
    self.refresh_log();
    egui::Panel::bottom("logs")
        .resizable(true)
        .default_size(180.0)
        .min_size(80.0)
        .max_size(ui.ctx().screen_rect().height() * 0.5)
        .show(ui, |ui| self.log_ui(ui));
}
```

In the status bar's right-to-left layout, after the `Unload` button, add a `Logs` toggle:

```rust
if ui.selectable_label(self.logs_open, "Logs").clicked() {
    self.logs_open = !self.logs_open;
    self.log_at = Instant::now() - Duration::from_secs(2);
}
```

- [ ] **Step 5: Build and verify manually**

Run: `cargo build --release`

Expected: clean build. Then load a model, click `Logs`, confirm the panel appears above the status bar and shows the server's output; resize it; scroll up and confirm it stops following; close and reopen it.

- [ ] **Step 6: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: llama-server log console at the bottom of the manager"
```

---

### Task 5: LHM probe ids and autostart

**Files:**
- Modify: `src/bin/quantdock.rs` (`LhmProbe`, `lhm_probe`, `wants_lhm_autostart`, probe channels, `poll`, `sensors_ui`, tests)

**Interfaces:**
- Consumes: `SensorsSettings.autostart` from Task 2.
- Produces: `struct LhmProbe { state: LhmState, ids: Vec<String> }`
- Produces: `fn lhm_probe(path: Option<&str>) -> LhmProbe`
- Produces: `fn wants_lhm_autostart(cfg: &Config) -> bool`
- Produces: `Manager.lhm_ids: Vec<String>`, `Manager.lhm_autostart_done: bool`.

- [ ] **Step 1: Write the failing tests**

Add a new `#[cfg(test)] mod tests` at the end of `src/bin/quantdock.rs`:

```rust
use quantdock::monitor_config::Row;

#[test]
fn autostart_requires_enabled_autostart_and_an_lhm_field() {
    let mut cfg = Config::defaults();
    cfg.sensors.enabled = true;
    cfg.sensors.autostart = true;
    cfg.monitor.rows = vec![Row { fields: vec!["gpu_hotspot".into()] }];
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
    cfg.monitor.rows = vec![Row { fields: vec!["gpu".into(), "ram".into()] }];
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --all-targets autostart_`

Expected: compile error, `wants_lhm_autostart` not found.

- [ ] **Step 3: Implement the condition and the probe**

```rust
fn wants_lhm_autostart(cfg: &Config) -> bool {
    cfg.sensors.enabled
        && cfg.sensors.autostart
        && cfg.monitor.rows.iter().any(|row| {
            row.fields.iter().any(|id| {
                quantdock::monitor_lhm::field_needs_lhm(id) || id.starts_with("lhm:")
            })
        })
}

struct LhmProbe {
    state: LhmState,
    ids: Vec<String>,
}

fn lhm_probe(path: Option<&str>) -> LhmProbe {
    if let Some(data) = quantdock::monitor_lhm::fetch_data() {
        return LhmProbe {
            state: LhmState::Connected,
            ids: data.all.into_keys().collect(),
        };
    }
    LhmProbe {
        state: if lhm_exe(path).is_some() {
            LhmState::InstalledNotRunning
        } else {
            LhmState::Missing
        },
        ids: Vec::new(),
    }
}
```

Replace every `lhm_probe_state(...)` call with `lhm_probe(...)`: the probe thread, the `Use this path` handler, and the post-download handler. Change `probe_tx`/`probe_rx` to carry `LhmProbe`, add `lhm_ids: Vec<String>` initialized to empty, and on receive set `self.lhm_state = probe.state; self.lhm_ids = probe.ids;`. Delete the synchronous `self.lhm_state = lhm_probe_state(...)` call in `poll()`; the 5 s thread is now the only reader.

- [ ] **Step 4: Add the one-shot autostart**

Add `lhm_autostart_done: bool` to `Manager` and compute the guard **before** the struct literal:

```rust
let autostart_at_launch = cfg.sensors.enabled && cfg.sensors.autostart;
// ... lhm_autostart_done: !autostart_at_launch,
```

This makes the guard true unless sensors and autostart were both already on at launch. Enabling either one mid-session never spawns LHM, even though the probe has not run yet; `Start it` stays the manual path.

In `poll()`, after draining `probe_rx`:

```rust
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
```

Add the checkbox in `sensors_ui` under the master switch:

```rust
changed |= ui
    .checkbox(
        &mut self.cfg.sensors.autostart,
        "Start LibreHardwareMonitor with QuantDock",
    )
    .changed();
```

- [ ] **Step 5: Run tests and verify manually**

Run: `cargo test --all-targets autostart_` — expected: 4 passed.

Then enable the master switch and autostart, Save, restart the manager, accept UAC, and confirm sensors become live without pressing `Start it`. Run again and cancel UAC: expect one note and no repeated prompt.

- [ ] **Step 6: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: start LibreHardwareMonitor with QuantDock"
```

---

### Task 6: Sensor picker

**Files:**
- Modify: `src/bin/quantdock.rs` (`PickerSensor`, `picker_list`, `add_picked_sensor`, state, channel, Custom sensors UI, tests)

**Interfaces:**
- Produces: `struct PickerSensor { id: String, text: String, unit: String, value: f64 }`
- Produces: `fn picker_list() -> Vec<PickerSensor>`
- Produces: `fn add_picked_sensor(cfg: &mut Config, row: usize, sensor: &PickerSensor) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
fn sample_sensor() -> PickerSensor {
    PickerSensor {
        id: "lhm:/gpu/0/temperature/0".into(),
        text: "GPU Core".into(),
        unit: "°C".into(),
        value: 40.0,
    }
}

#[test]
fn picker_add_does_not_duplicate_a_field() {
    let mut cfg = Config::defaults();
    let sensor = sample_sensor();
    assert!(add_picked_sensor(&mut cfg, 0, &sensor));
    assert!(!add_picked_sensor(&mut cfg, 0, &sensor));
    assert_eq!(
        cfg.monitor.rows[0]
            .fields
            .iter()
            .filter(|f| *f == &sensor.id)
            .count(),
        1
    );
    assert_eq!(cfg.monitor.custom[&sensor.id], "GPU CORE");
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --all-targets picker_`

Expected: compile error, `PickerSensor` and `add_picked_sensor` not found.

- [ ] **Step 3: Implement the data and add helper**

```rust
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

fn add_picked_sensor(cfg: &mut Config, row: usize, sensor: &PickerSensor) -> bool {
    let Some(target) = cfg.monitor.rows.get_mut(row) else {
        return false;
    };
    if target.fields.iter().any(|f| f == &sensor.id) {
        return false;
    }
    target.fields.push(sensor.id.clone());
    let label: String = sensor.text.to_uppercase().chars().take(12).collect();
    cfg.monitor.custom.entry(sensor.id.clone()).or_insert(label);
    true
}
```

- [ ] **Step 4: Add the picker UI**

Add `picker_open: bool`, `picker_sensors: Vec<PickerSensor>`, `picker_filter: String`, `picker_target: usize`, `picker_busy: bool`, `picker_tx: std::sync::mpsc::Sender<Vec<PickerSensor>>`, `picker_rx: std::sync::mpsc::Receiver<Vec<PickerSensor>>` to `Manager`. Compute the default target before the struct literal: `let picker_target = cfg.monitor.rows.len().saturating_sub(1);`. In `poll()`, drain `picker_rx` into `picker_sensors` and clear `picker_busy`.

In the Custom sensors section, after the paste row:

- A `Browse sensors` toggle button. Every transition from closed to open spawns `picker_list()` on a thread and sends the result through `picker_tx`; set `picker_busy = true`. `Refresh` does the same, so a reopened list is never stale.
- When open, clamp `self.picker_target = self.picker_target.min(self.cfg.monitor.rows.len().saturating_sub(1));`, then show `Refresh`, a search box bound to `picker_filter`, and an `Add to: Row N` combo over `1..=rows.len()`, defaulting to the last row. Disable `Add` when there are no rows.
- A `ScrollArea::vertical().max_height(200.0)` list filtered case-insensitively on text and id. Each entry shows the text, `value unit`, the id in weak monospace, and an `Add` button.
- Collect the clicked sensor into `Option<PickerSensor>` (clone it; the derive exists for this), then after the list call `changed |= add_picked_sensor(&mut self.cfg, target, &sensor)`. `add_picked_sensor` returns `false` for a row that no longer exists.
- If `picker_sensors` is empty: `No sensors reported. Start LibreHardwareMonitor, then Refresh.`
- Keep the paste box unchanged as the fallback.

- [ ] **Step 5: Run tests and verify manually**

Run: `cargo test --all-targets picker_` — expected: 2 passed.

Then open Settings -> Custom sensors -> `Browse sensors`, search `temperature`, add one to a row, Save, and confirm it appears in the overlay with a live value.

- [ ] **Step 6: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: pick custom sensors from a live LHM list"
```

---

### Task 7: Missing-sensor marker

**Files:**
- Modify: `src/bin/quantdock.rs` (custom sensor list, `is_missing`, tests)

**Interfaces:**
- Consumes: `Manager.lhm_ids` and `LhmState` from Task 5.
- Produces: `fn is_missing(known: &[String], id: &str) -> bool`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn missing_only_when_the_id_is_absent() {
    let known = vec!["lhm:/gpu/0/temperature/0".to_string()];
    assert!(!is_missing(&known, "lhm:/gpu/0/temperature/0"));
    assert!(is_missing(&known, "lhm:/gpu/0/temperature/1"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --all-targets missing_only`

Expected: compile error, `is_missing` not found.

- [ ] **Step 3: Implement the marker**

```rust
fn is_missing(known: &[String], id: &str) -> bool {
    !known.iter().any(|k| k == id)
}
```

In the Custom sensors list, when `self.lhm_state == LhmState::Connected` and `is_missing(&self.lhm_ids, &id)`, show an amber `(missing)` label next to the id. Do not change the id or remap it.

- [ ] **Step 4: Run the test and verify manually**

Run: `cargo test --all-targets missing_only` — expected: 1 passed.

With LHM connected, a sensor the machine does not report should be marked; with LHM stopped, no marker should appear.

- [ ] **Step 5: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: mark custom sensors missing on this machine"
```

---

### Task 8: Dirty draft vs. disk change

**Files:**
- Modify: `src/bin/quantdock.rs` (`Manager.disk_changed`, `poll`, `settings_ui`)

**Interfaces:**
- Produces: `Manager.disk_changed: bool`, cleared by Save and Return.

- [ ] **Step 1: Add the state and consume the watcher while dirty**

Add `disk_changed: bool` to `Manager`, initialized `false`. Replace the watcher block in `poll()`:

```rust
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
```

- [ ] **Step 2: Show the notice and clear it**

In `settings_ui`, next to the dirty marker, show when `self.disk_changed`:

```rust
ui.colored_label(
    egui::Color32::from_rgb(230, 190, 80),
    "settings.toml changed on disk — Return reloads it, Save overwrites it.",
);
```

Set `self.disk_changed = false` in the successful Save branch and after the Return reload.

- [ ] **Step 3: Build and verify manually**

Run: `cargo build --release`

Expected: clean build. With unsaved Settings changes, edit `%APPDATA%\QuantDock\settings.toml` in an editor; the notice appears. `Return` reloads the file; `Save` overwrites it. Neither leaves the notice behind.

- [ ] **Step 4: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat: warn when settings.toml changes under an unsaved draft"
```

---

### Task 9: README, full verification, memory plateau

**Files:**
- Modify: `README.md`

**Interfaces:** none.

- [ ] **Step 1: Update the README**

- Using it: add a `Logs` bullet — opens a bottom console with the last 200 lines of the current model's `llama-server.log`; it stays on the last loaded model after the server exits.
- Hardware sensors: add the `Start LibreHardwareMonitor with QuantDock` checkbox, default off, UAC at launch.
- Custom sensors: describe `Browse sensors`, the searchable list and the target row; keep the paste fallback.
- Tests: mention the log-tail and migration coverage.

- [ ] **Step 2: Run the full suite and release build**

Run: `cargo test --all-targets`

Expected: all tests pass, ignored tests skipped.

Run: `cargo build --release`

Expected: clean build, both `quantdock.exe` and `monitor.exe`.

- [ ] **Step 3: Run the 10-minute memory plateau**

Start the release manager and overlay with sensors enabled at current defaults. Sample private commit and working set every 30 s for 10 minutes.

Pass: after the first 2 minutes, growth ≤ 2 MB/min and no monotonic climb across the final 5 minutes. Fail: take a Windows heap trace before changing code.

- [ ] **Step 4: Review, push, confirm CI**

Review the full range for the Review Focus items and any regressions; fix Critical and Important findings with tests. Then:

```bash
git add -A
git commit -m "docs: document the log console, sensor picker and LHM autostart"
git push origin main
```

Expected: the `build` workflow green on the pushed commit.
