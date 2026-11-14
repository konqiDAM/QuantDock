# Custom Sensors — Design

Date: 2026-11-14
Status: awaiting approval
Builds on: `2026-11-14-monitor-config-design.md`

## Intent

LibreHardwareMonitor exposes far more than the overlay currently shows — GPU clocks,
voltages, PCIe throughput, per-core clocks, SSD wear, network rates. Which ones exist
depends on the machine. Rather than hard-coding a growing list of fields, let the user
pick **any** sensor LHM reports and place it in a row like a built-in field.

## Goals

- List every sensor LHM is currently reporting, with its value, inside the row builder.
- Add one to a row in a click; it behaves like a built-in field (thresholds, bars,
  graphs, hot reload, `--` when unavailable).
- Give it a short editable label (`GPU CLK`), because LHM's names are long.
- Keep working when the sensor disappears (different GPU, driver update): show `--`,
  never crash, never silently show another sensor's value.

## Non-goals

- Sensors LHM does not expose (no inventing values).
- Unit conversion or renaming units; the unit is whatever LHM reports.
- Editing a sensor's own settings in LHM (fan curves, offsets).
- Per-machine presets or sharing sensor layouts between machines.

## Design

### Identity

A custom field's id is its LHM `SensorId` with an `lhm:` prefix:

```
lhm:/nvidiagpu/0/clock/1
lhm:/amdcpu/0/temperature/2
```

The prefix keeps built-in ids (`gpu`, `cpu_temp`) unambiguous. `SensorId` is stable
across restarts and machines for the same hardware family, and it is what LHM itself
uses.

### Config

```toml
[monitor.custom]
# SensorId -> short label shown in the overlay
"lhm:/nvidiagpu/0/clock/1" = "GPU CLK"
"lhm:/nvidiagpu/0/clock/2" = "MEM CLK"
```

Rows reference them like any field:

```toml
[[monitor.rows]]
fields = ["gpu", "lhm:/nvidiagpu/0/clock/1", "lhm:/nvidiagpu/0/clock/2"]
```

A custom id with no label falls back to the sensor's own `Text` (truncated to 12
characters). Thresholds (`[thresholds."lhm:..."]`) and `field_style` entries work
unchanged, keyed by the same string.

### Sampling

`monitor_lhm::fetch()` grows a second output: every sensor with a parseable number,
keyed by `SensorId`:

```rust
pub struct SensorValue {
    pub text: String,      // LHM's own text, e.g. "GPU Core"
    pub unit: String,      // "MHz", "°C", "V", "W", "%", "RPM", "MB/s"
    pub value: f64,
}

pub fn fetch_all() -> Option<BTreeMap<String, SensorValue>>
```

`Sample` gains `custom: BTreeMap<String, SensorValue>`, filled once per tick from the
same HTTP response — no extra request. Only sensors with a numeric value are kept;
text sensors are ignored.

### Rendering

`field_def(id)` keeps returning built-ins. A new resolver turns either kind into what the
renderer needs:

```rust
enum Resolved<'a> { Builtin(&'static FieldDef), Custom(FieldView<'a>) }
struct FieldView<'a> { id: &'a str, label: &'a str, kind: FieldKind }
```

Unit → kind for custom sensors: `°C`→Celsius, `V`→Volts (new, `"{:.2} V"`), `W`→Watts,
`MHz`→Mhz, `RPM`→Rpm, `%`→Percent, anything else→Number (with the unit appended).

Bars stay restricted to a natural 0..1 scale: `%` and level-style units. Graphs work for
any numeric sensor. A missing sensor renders `--` in the label colour, exactly like a
built-in with no data.

### Settings UI

In the row builder's **+ field** menu, after the built-in vocabulary, a divider and a
scrollable list of every sensor LHM is currently reporting:

```
GPU / GPU Core           52 °C
GPU / GPU Memory Clock   7001 MHz
CPU / CPU Core #1        2804 MHz
SSD / Temperature        43 °C
```

Clicking one appends `lhm:<SensorId>` to that row and writes a default label from
`SensorValue::text`. The **field table** gains an editable label cell for custom ids, so
`GPU Memory Clock` becomes `MEM CLK`.

If sensors are not connected, the menu section shows "connect hardware sensors to pick
from N available" instead of an empty list.

### Failure and portability

- LHM not running → no `custom` entries → every custom field shows `--`.
- A stored id that is no longer reported → `--`, and the field table marks it
  `missing on this machine`. It is never remapped to a different sensor.
- Duplicate labels are allowed; ids are the identity.

## Approaches considered

1. **Fixed extra fields** (`gpu_clock`, `pcie_rx`, `disk_temp`, …). Simple, but incomplete
   by construction and needs a release per sensor.
2. **Custom sensor picker (chosen).** One mechanism, every sensor, no hardware guessing.
   Cost: dynamic field ids in `Sample` and the resolver.
3. **Free-text sensor id** — power without a picker, but requires knowing LHM's id
   strings; kept as an implicit fallback because the config is hand-editable.

## Constraints

- No new dependency; the parser already walks the JSON tree.
- No extra HTTP request per tick.
- Built-in fields and existing configs unchanged.
- `Sample` keys become `String`; the change is mechanical but touches every insert.

## Review focus

- LHM reporting two sensors with the same `Text` (`GPU Memory` clock vs temperature) —
  identity is the id, never the text.
- A custom id typed by hand that does not exist → `--`, not a panic.
- Percent sensors get bars; `MHz` and `V` do not.
- A sensor that disappears between ticks → the field goes to `--` on the next tick.
- Labels longer than the panel: auto-width grows; the fallback label is truncated by
  characters, not bytes.

## Left to the builder

- Whether the picker groups by hardware (GPU / CPU / SSD) or is one flat searchable list.
- Whether adding a sensor also creates a row when the target row is full.
- Exact decimals per unit (`7001 MHz` vs `7.0 GHz`).
