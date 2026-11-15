# Sensor tree in the sensor window

Date: 2026-11-15
Status: approved (type-first shape selected in conversation)
Builds on: `2026-11-15-sensor-window-and-profiles-design.md`

## Intent

The sensor window currently flattens every LibreHardwareMonitor sensor into one
long list. A row that says `CPU Package`, `GPU Hot Spot` or `Temperature #1`
does not say which hardware it belongs to. Replace that flat section with a
type-first tree:

```
CPU   Intel Xeon E5-2696 v4
  Temperatures (47)
    [x] CPU Package                 35 °C
    [ ] Core Max                    38 °C
  Powers (4)
  Clocks (23)
  Load (46)
  Voltages (23)

GPU   NVIDIA GeForce RTX 3090
  Temperatures (3)
    [x] GPU Core                    52 °C
    [ ] GPU Hot Spot                72 °C
    [ ] GPU Memory Junction         66 °C
  Load (20)
  Fans (2)
```

The top level is the hardware type: CPU, GPU, Memory, Board, Storage, Network,
Other. Under it is the actual hardware name from LHM, then the LHM sensor group
(Temperatures, Powers, Clocks, Load, Voltages, Fans, Controls, Data,
Throughput), then the sensor row. Board's nested hardware (board model then
`Nuvoton NCT6791D`) stays nested.

## Current state

- `monitor_lhm::parse_data` walks the LHM tree and flattens every numeric sensor
  into `LhmData.all`. It keeps the sensor text, unit and value, but drops the
  parent hardware and group names.
- `PickerSensor` has only `id`, `text`, `unit`, `value`; `sensor_window_ui`
  renders one flat list under a `LibreHardwareMonitor` heading.
- On this machine LHM reports `/intelcpu/0` (Intel Xeon E5-2696 v4),
  `/gpu-nvidia/0` (NVIDIA GeForce RTX 3090), `/ram`, `/vram`, `/motherboard`
  with a nested `/lpc/nct6791d/0` (Nuvoton NCT6791D), `/nvme`, `/hdd`, `/ssd`
  and `/nic` hardware. Those names are available in `data.json` as `Text`
  beside `HardwareId`.

## Goals

1. Group live LHM sensors as type -> hardware -> LHM group -> sensor.
2. Keep the hardware display names and the LHM group names. Nested hardware
   paths stay nested.
3. Keep the existing checkbox, live value/unit and `SensorId` hover behavior.
4. Search matches sensor text, id, group and hardware path; matching branches
   open automatically.
5. Default view opens CPU and GPU, with their Temperatures group open; other
   categories and groups stay collapsed.
6. Built-in fields stay a separate flat block above the tree. Stored/missing
   sensors stay a separate block below it.
7. Do not add metadata to the sampler's hot path. The tree is parsed only when
   the picker opens or refreshes.

## Non-goals

- Grouping the built-in field vocabulary into the hardware tree.
- Grouping stored/missing sensors into the live tree.
- Persisting expansion state across app runs.
- Changing overlay rows, labels or `FieldView`.
- New dependencies or a second HTTP client.

## Approaches

**Extend `SensorValue` vs parse the tree only for the picker.** Extending
`SensorValue` with hardware/group strings would add allocations to
`LhmData.all` on every sampler refresh (about 300 sensors every 2 s), which the
memory plan is trying to avoid. A separate picker-only `fetch_tree()` makes one
request when the window opens or refreshes and leaves the sampler's `Reader`,
`LhmData` and `SensorValue` untouched. Chosen: picker-only.

**Type-first vs hardware-first.** Hardware-first is the exact LHM tree, but
`CPU` and `GPU` become small badges and many storage/NIC devices compete at the
same level. Type-first puts CPU and GPU first, matches the user's "under a tree
with that name", and keeps storage/network collapsed. Chosen: type-first.

## Design

### 1. Picker-only tree parser

In `monitor_lhm.rs`:

```rust
pub struct SensorItem {
    pub id: String,          // "lhm:<SensorId>"
    pub text: String,
    pub unit: String,
    pub value: f64,
    pub hardware: Vec<String>, // display path, e.g. ["ASUS ...", "Nuvoton ..."]
    pub hardware_id: String,   // leaf HardwareId, e.g. "/lpc/nct6791d/0"
    pub group: String,         // "Temperatures"
}

pub fn parse_tree(json: &str) -> Option<Vec<SensorItem>>;
pub fn fetch_tree() -> Option<Vec<SensorItem>>;
```

`Node` gains `hardware_id: Option<&str>` (`HardwareId`). A shared
`reading(node) -> Option<(f64, String)>` extracts the numeric raw value and the
unit so the flat `walk` and the tree `walk_tree` agree.

`walk_tree` carries:
- `hardware: Vec<String>` — push on a hardware node, pop on exit.
- `hardware_id: String` — replace on a hardware node, restore on exit.
- `group: String` — set by a group node, restore on exit; a hardware node
  resets it to empty.

A sensor node emits one `SensorItem` with a clone of the current hardware path.
A sensor with no numeric value is skipped. An empty hardware path falls back to
`["Other"]`; an empty group falls back to `Other`.

`fetch_tree` uses the existing `agent()` and `URL`, so there is no new
dependency and no second connection pool.

### 2. Category and tree model

In `quantdock.rs`, `PickerSensor` gains the three metadata fields. `picker_list`
maps `monitor_lhm::fetch_tree()` into it; it no longer creates a `Reader`.

`hardware_category(hardware_id)` maps:
- `/intelcpu`, `/amdcpu`, `/cpu` -> CPU
- `/gpu-nvidia`, `/nvidiagpu`, `/atigpu`, `/gpu-amd`, `/gpu` -> GPU
- `/ram`, `/vram` -> Memory
- `/motherboard`, `/lpc` -> Board
- `/nvme`, `/hdd`, `/ssd`, `/storage` -> Storage
- `/nic` -> Network
- anything else -> Other

Fixed display order: CPU, GPU, Memory, Board, Storage, Network, Other.

`build_sensor_tree(sensors, needle) -> Vec<SensorCategory>` filters by sensor
text, id, group and any hardware path segment, then groups. Categories sort by
the fixed order; hardware and groups keep the LHM order. A group header count is
the number of matching sensors in it.

### 3. Rendering

`CollapsingHeader` for category -> hardware -> group. Nested hardware paths
render one header per path segment. CPU and GPU default open, and their
Temperatures group defaults open; everything else is collapsed. A non-empty
search forces every matching level open.

Leaf rows keep the current checkbox, value/unit and hover id. The same deferred
`toggle` action applies. Built-in fields and stored/missing blocks stay where
they are.

## Tests

- `tree_keeps_hardware_group_and_units`
- `tree_keeps_nested_board_path`
- `tree_skips_non_numeric_sensors`
- `hardware_category_maps_lhm_prefixes`
- `build_sensor_tree_groups_by_category`
- `build_sensor_tree_filters_by_parent_names`
- `build_sensor_tree_falls_back_for_empty_hardware`
- Existing 88 tests stay green; 4 ignored. Expected total after this work: 95
  (78 lib + 17 bin).

## Review focus

- Both `/gpu-nvidia` (live) and `/nvidiagpu` (existing tests) map to GPU.
- A board sensor keeps both the board model and the nested LPC chip in its path.
- A sensor with no hardware path and a sensor with no group still appear under
  Other, never disappear.
- Search "temperatures" shows every temperature group; search "nvidia" shows
  only GPU hardware; a search with no match shows a message, not an empty tree.
- LHM stopped: built-ins and stored/missing still render; the tree area says
  start LHM and refresh.
- Duplicate header labels across CPU/GPU do not collide (`id_salt`/`push_id`).
- No sampler allocation or `SensorValue` change.

## Constraints

- Windows-only, no new dependencies, no new HTTP agent.
- The picker worker still fetches off the UI thread; no polling while open.
- The crash remains unresolved unless reproduced; do not claim a fix.
- The memory waiver is unchanged.

## What is left to the builder

- Exact indentation and header counts; the tree shape is fixed, spacing is not.
- Whether to show a category count in the header; default is no.
- The exact text of the no-match message.
