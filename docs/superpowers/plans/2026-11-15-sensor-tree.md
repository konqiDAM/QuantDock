# Hardware-Grouped Sensor Tree Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the flat LibreHardwareMonitor sensor list with a type-first tree: CPU/GPU/Memory/Board/Storage/Network -> hardware name -> LHM sensor group -> sensor.

**Architecture:** Add a picker-only `fetch_tree()` in `monitor_lhm` that walks the existing LHM JSON tree and keeps the hardware path and group name. `quantdock` maps those items into an extended `PickerSensor`, builds a filtered category/hardware/group tree, and renders it with `CollapsingHeader`s. The sampler's `Reader`, `LhmData` and `SensorValue` are untouched.

**Tech Stack:** Rust 2024, egui/eframe 0.37, existing ureq agent and LHM `data.json`.

**Spec:** `docs/superpowers/specs/2026-11-15-sensor-tree-design.md`

## Global Constraints

- Windows-only, no new dependencies, no second HTTP agent.
- The picker fetch stays on its worker thread; no polling while the window is open.
- `LhmData`, `SensorValue`, `Reader` and the sampler stay unchanged; no added allocations per sampler tick.
- Built-in fields stay a separate flat block; stored/missing sensors stay a separate block.
- Type order is CPU, GPU, Memory, Board, Storage, Network, Other.
- CPU and GPU default open; their Temperatures group defaults open. Everything else is collapsed.
- Search matches sensor text, id, group and hardware path, and opens matching branches.
- The crash remains unresolved unless reproduced; do not claim a fix.
- Baseline: 88 passing tests (75 lib + 13 bin), 4 ignored. Expected after this plan: 95 (78 lib + 17 bin), 4 ignored.

## Review Focus

- Both `/gpu-nvidia` (live) and `/nvidiagpu` (tests) map to GPU.
- A board sensor keeps both the board model and the nested LPC chip in its path.
- A sensor with no hardware path or no group appears under Other, never disappears.
- Search by group (`temperatures`), hardware (`nvidia`) and sensor text (`cpu package`) includes the right leaves and excludes the rest.
- LHM stopped still leaves built-ins and stored/missing usable.
- Duplicate header labels across CPU/GPU do not collide (`id_salt`).
- The sampler's `Reader`/`LhmData` path has no new metadata work.

---

### Task 1: Picker-only tree parser in `monitor_lhm`

**Files:**
- Modify: `src/monitor_lhm.rs`

**Interfaces:**
- Produces: `pub struct SensorItem { pub id: String, pub text: String, pub unit: String, pub value: f64, pub hardware: Vec<String>, pub hardware_id: String, pub group: String }`, deriving `Debug, Clone, PartialEq`.
- Produces: `pub fn parse_tree(json: &str) -> Option<Vec<SensorItem>>`
- Produces: `pub fn fetch_tree() -> Option<Vec<SensorItem>>`
- Produces private `fn reading(node: &Node<'_>) -> Option<(f64, String)>` and `fn walk_tree(...)`.
- Adds `hardware_id: Option<&'a str>` to `Node` (`HardwareId`).
- Does not change `SensorValue`, `LhmData`, `Reader`, `parse_data` or `walk`.

- [ ] **Step 1: Write the failing tests**

Add these tests inside `mod tests` in `src/monitor_lhm.rs`:

```rust
#[test]
fn tree_keeps_hardware_group_and_units() {
    let json = r#"{"Children":[
      {"Text":"CPU","HardwareId":"/intelcpu/0","Children":[
        {"Text":"Temperatures","Children":[
          {"Text":"CPU Package","SensorId":"/intelcpu/0/temperature/4","Value":"35,0 °C"}]}]},
      {"Text":"GPU","HardwareId":"/gpu-nvidia/0","Children":[
        {"Text":"Temperatures","Children":[
          {"Text":"GPU Hot Spot","SensorId":"/gpu-nvidia/0/temperature/2","Value":"72,3 °C"}]}]}]}"#;

    let items = parse_tree(json).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, "lhm:/intelcpu/0/temperature/4");
    assert_eq!(items[0].text, "CPU Package");
    assert_eq!(items[0].unit, "°C");
    assert_eq!(items[0].value, 35.0);
    assert_eq!(items[0].hardware, vec!["CPU"]);
    assert_eq!(items[0].hardware_id, "/intelcpu/0");
    assert_eq!(items[0].group, "Temperatures");
    assert_eq!(items[1].hardware, vec!["GPU"]);
}

#[test]
fn tree_keeps_nested_board_path() {
    let json = r#"{"Children":[
      {"Text":"Board","HardwareId":"/motherboard","Children":[
        {"Text":"Nuvoton NCT6791D","HardwareId":"/lpc/nct6791d/0","Children":[
          {"Text":"Temperatures","Children":[
            {"Text":"Temperature #1","SensorId":"/lpc/nct6791d/0/temperature/0","Value":"42,5 °C"}]}]}]}]}"#;

    let items = parse_tree(json).unwrap();
    assert_eq!(items[0].hardware, vec!["Board", "Nuvoton NCT6791D"]);
    assert_eq!(items[0].hardware_id, "/lpc/nct6791d/0");
    assert_eq!(items[0].group, "Temperatures");
}

#[test]
fn tree_skips_non_numeric_sensors() {
    let json = r#"{"Children":[
      {"Text":"CPU","HardwareId":"/intelcpu/0","Children":[
        {"Text":"Temperatures","Children":[
          {"Text":"N/A","SensorId":"/intelcpu/0/temperature/9","Value":"N/A"}]}]}]}"#;

    assert!(parse_tree(json).is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib monitor_lhm::tests::tree_keeps_hardware_group_and_units`

Expected: FAIL to compile with `cannot find function parse_tree` and `cannot find type SensorItem`.

- [ ] **Step 3: Add the tree types and node field**

Add `#[serde(rename = "HardwareId", default)] hardware_id: Option<&'a str>,` to `Node`.

Add above `SensorValue`:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct SensorItem {
    pub id: String,
    pub text: String,
    pub unit: String,
    pub value: f64,
    pub hardware: Vec<String>,
    pub hardware_id: String,
    pub group: String,
}
```

Add the shared reading helper next to `num`:

```rust
fn reading(node: &Node<'_>) -> Option<(f64, String)> {
    let raw = if node.raw.is_empty() {
        node.value.as_ref()
    } else {
        node.raw.as_ref()
    };
    let value = num(raw)?;
    let unit = node
        .value
        .trim()
        .rsplit_once(' ')
        .map(|(_, unit)| unit.trim().to_owned())
        .unwrap_or_default();
    Some((value, unit))
}
```

In the existing flat `walk`, keep `*sensors += 1;` and the `value`/`raw` bindings.
Replace only the old `if let Some(n) = num(raw) { ... }` value/unit block with the
shared helper:

```rust
    if let Some(id) = node.sensor_id {
        *sensors += 1;
        let value = node.value.as_ref();
        let raw = if node.raw.is_empty() {
            value
        } else {
            node.raw.as_ref()
        };
        apply(id, node.text.as_ref(), value, raw, &mut out.named);
        if let Some((value, unit)) = reading(node) {
            out.all.insert(
                format!("lhm:{id}"),
                SensorValue {
                    text: node.text.as_ref().to_owned(),
                    unit,
                    value,
                },
            );
        }
    }
```

- [ ] **Step 4: Implement `walk_tree`, `parse_tree` and `fetch_tree`**

Add after `walk`:

```rust
fn walk_tree<'a>(
    node: &'a Node<'a>,
    hardware: &mut Vec<String>,
    hardware_id: &mut String,
    group: &mut String,
    out: &mut Vec<SensorItem>,
) {
    if let Some(id) = node.hardware_id {
        hardware.push(node.text.to_string());
        let previous_id = std::mem::replace(hardware_id, id.to_owned());
        let previous_group = std::mem::take(group);
        for child in &node.children {
            walk_tree(child, hardware, hardware_id, group, out);
        }
        hardware.pop();
        *hardware_id = previous_id;
        *group = previous_group;
        return;
    }

    if let Some(sensor_id) = node.sensor_id {
        if let Some((value, unit)) = reading(node) {
            out.push(SensorItem {
                id: format!("lhm:{sensor_id}"),
                text: node.text.to_string(),
                unit,
                value,
                hardware: if hardware.is_empty() {
                    vec!["Other".to_owned()]
                } else {
                    hardware.clone()
                },
                hardware_id: hardware_id.clone(),
                group: if group.is_empty() {
                    "Other".to_owned()
                } else {
                    group.clone()
                },
            });
        }
        return;
    }

    if !node.children.is_empty() {
        let previous = if node.text.is_empty() || node.text.as_ref() == "Sensor" {
            group.clone()
        } else {
            std::mem::replace(group, node.text.to_string())
        };
        for child in &node.children {
            walk_tree(child, hardware, hardware_id, group, out);
        }
        *group = previous;
    }
}

pub fn parse_tree(json: &str) -> Option<Vec<SensorItem>> {
    let root: Node = serde_json::from_str(json).ok()?;
    let mut items = Vec::new();
    let mut hardware = Vec::new();
    let mut hardware_id = String::new();
    let mut group = String::new();
    walk_tree(&root, &mut hardware, &mut hardware_id, &mut group, &mut items);
    (!items.is_empty()).then_some(items)
}

pub fn fetch_tree() -> Option<Vec<SensorItem>> {
    let mut resp = agent().get(URL).call().ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    parse_tree(&text)
}
```

- [ ] **Step 5: Run the LHM and full tests**

Run: `cargo test --lib monitor_lhm`

Expected: PASS, including the three new tests and the existing flat parser tests.

Run: `cargo test --all-targets`

Expected: PASS, 78 lib + 13 bin = 91 passed, 4 ignored.

Run: `cargo build --release`

Expected: PASS, both exes, no warnings.

- [ ] **Step 6: Commit**

```bash
git add src/monitor_lhm.rs
git commit -m "feat(lhm): expose a grouped sensor tree for the picker"
```

---

### Task 2: Picker metadata and tree builder

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Changes `PickerSensor` to add `pub hardware: Vec<String>`, `pub hardware_id: String`, `pub group: String`.
- Changes `picker_list()` to use `quantdock::monitor_lhm::fetch_tree()` and no longer construct a `Reader`.
- Produces: `fn hardware_category(hardware_id: &str) -> &'static str`
- Produces private `struct SensorCategory { name: &'static str, hardware: Vec<SensorHardware> }`
- Produces private `struct SensorHardware { path: Vec<String>, groups: Vec<SensorGroupView> }`
- Produces private `struct SensorGroupView { name: String, sensors: Vec<usize> }`
- Produces private `fn build_sensor_tree(sensors: &[PickerSensor], needle: &str) -> Vec<SensorCategory>`
- Produces private `fn sensor_matches(sensor: &PickerSensor, needle: &str) -> bool`
- Does not change the sensor window rendering yet; the flat list still compiles using the extra fields.

- [ ] **Step 1: Write the failing tests**

Add `SensorItem`-shaped metadata to the test helper and add these tests. Use a helper:

```rust
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
```

Replace `sample_sensor()` with:

```rust
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
```

Update `picker_label_is_uppercase_and_short` to construct through `picker(...)` with the same id/text/unit/value and empty metadata.

Add:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin quantdock hardware_category_maps_lhm_prefixes`

Expected: FAIL to compile with `cannot find function hardware_category` and `cannot find function build_sensor_tree`.

- [ ] **Step 3: Extend `PickerSensor` and `picker_list`**

```rust
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
```

- [ ] **Step 4: Add category mapping and the tree builder**

Add next to `custom_label`:

```rust
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
```

- [ ] **Step 5: Run the tests and build**

Run: `cargo test --bin quantdock`

Expected: PASS, including the four new tests and the updated picker tests.

Run: `cargo test --all-targets`

Expected: PASS, 78 lib + 17 bin = 95 passed, 4 ignored.

Run: `cargo build --release`

Expected: PASS, no warnings. The flat sensor window still compiles with the extra fields.

- [ ] **Step 6: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat(settings): build a hardware-grouped sensor tree"
```

---

### Task 3: Render the tree in the sensor window

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Consumes: `build_sensor_tree`, `SensorCategory`, `SensorHardware`, `SensorGroupView`, extended `PickerSensor`.
- Produces: `fn sensor_tree_ui(ui: &mut egui::Ui, cfg: &Config, sensors: &[PickerSensor], needle: &str, rows_empty: bool, toggle: &mut Option<(String, bool)>)`
- Produces: `fn render_sensor_hardware(ui: &mut egui::Ui, cfg: &Config, sensors: &[PickerSensor], category: &str, path: &[String], hardware: &SensorHardware, needle: &str, rows_empty: bool, toggle: &mut Option<(String, bool)>)`
- Changes only the LibreHardwareMonitor section of `sensor_window_ui`; the built-in loop and stored/missing block stay.

- [ ] **Step 1: Add the tree renderers**

Add these free functions near `build_sensor_tree`:

```rust
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
                        || (matches!(category, "CPU" | "GPU")
                            && group.name == "Temperatures");
                    egui::CollapsingHeader::new(format!(
                        "{} ({})",
                        group.name,
                        group.sensors.len()
                    ))
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
                                    .add_enabled(
                                        can,
                                        egui::Checkbox::new(&mut on, &sensor.text),
                                    )
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
```

- [ ] **Step 2: Replace the flat LHM loop**

In `sensor_window_ui`, inside the `LibreHardwareMonitor` section, replace the
`for sensor in &self.picker_sensors { ... }` loop with:

```rust
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
```

Keep the built-in fields loop above and the stored/missing block below unchanged.

- [ ] **Step 3: Run the tests and build**

Run: `cargo test --all-targets`

Expected: PASS, 95 passed + 4 ignored.

Run: `cargo build --release`

Expected: PASS, no warnings.

- [ ] **Step 4: Smoke the window**

Run `target\release\quantdock.exe`, open Settings -> **Configure available
sensors…**, and check:
- CPU and GPU appear first; their Temperatures groups are open.
- A board sensor sits under `Board -> <board model> -> <LPC chip> -> Temperatures`.
- Search `nvidia` leaves only GPU; search `temperatures` shows temperature groups; search `cpu package` shows the CPU package sensors.
- Checking a tree leaf adds it to the target row; unchecking removes it.
- LHM stopped: built-ins and stored/missing still render, with the start-LHM hint.
- `%APPDATA%\QuantDock\crash.log` has no new timestamp.

- [ ] **Step 5: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "feat(settings): render the sensor window as a hardware tree"
```

---

### Task 4: Documentation and final verification

**Files:**
- Modify: `README.md`
- Modify: `INSTALL.md`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: the final tree behavior from Tasks 1–3.

- [ ] **Step 1: Document the tree**

In `README.md`, after the existing sentence that says the window lists every
built-in field and every sensor, add:

```markdown
Live LibreHardwareMonitor sensors are grouped the way the monitor shows them:
CPU, GPU, Memory, Board, Storage and Network, then the hardware name, then
Temperatures, Powers, Clocks, Load, Voltages, Fans, Controls, Data or
Throughput. Nested board chips stay nested. CPU and GPU open first, with their
Temperatures group; the other categories stay collapsed. Search matches sensor
names, ids, groups and hardware names and opens matching branches.
```

In `INSTALL.md`, in `## Overlay setup (optional)`, add:

```markdown
The sensor window groups live sensors by CPU, GPU, memory, board, storage and
network, then by sensor type, so a `CPU Package` temperature is shown under its
own hardware.
```

- [ ] **Step 2: Update AGENTS.md**

- Code map `src/monitor_lhm.rs`: `LibreHardwareMonitor HTTP client, data.json parser, reusable Reader, picker-only hardware tree.`
- Code map `src/bin/quantdock.rs`: `Manager UI: model cards, Settings draft, sensor manager window (hardware tree), profiles, field tables, themes, preview, LHM setup, log console.`
- Current state: add `- Hardware-grouped sensor tree (CPU/GPU/Memory/Board/Storage/Network).`
- Commands test count: `# 95 tests: 78 lib + 17 bin, 4 ignored`

- [ ] **Step 3: Run the full verification**

```bash
cargo test --all-targets
cargo build --release
```

Expected: 95 passed, 4 ignored, zero warnings.

Launch `target\release\quantdock.exe` for a 5-second smoke test, close it
cleanly, and confirm `%APPDATA%\QuantDock\crash.log` did not change.

- [ ] **Step 4: Commit**

```bash
git add README.md INSTALL.md AGENTS.md
git commit -m "docs: document the grouped sensor tree"
```

---

## Self-Review

- Spec coverage: picker-only tree parser (Task 1), category/tree model and search (Task 2), rendering/default-open/nested hardware (Task 3), docs and verification (Task 4).
- No sampler or `SensorValue` changes; no new dependencies.
- The type-first order and CPU/GPU default-open rules are pinned.
- Test counts: 88 + 3 lib + 4 bin = 95 (78 lib + 17 bin, 4 ignored).
- All new types/functions used by later tasks are named in Interfaces.
- The crash remains unresolved; Task 3 Step 4 and Task 4 Step 3 check for a new log, not a fix.
