# QuantDock

A lightweight desktop manager for local GGUF models (llama.cpp / `llama-server`) on Windows.

## What it does

- **Load / unload** models from one list, no launcher scripts
- **Per-model config files** — tune context, sampling, KV cache and speculative decoding by
  editing one `.toml`
- **Shows what is *actually* running** — llama-server accepts any model id, so the label is
  not proof. QuantDock reads `/props` and resolves the real GGUF.
- **Floating resource monitor** — transparent, always-on-top, with rows you arrange yourself
- **Three OpenCode entries** — `local`, `local-3d`, `local-twil`, one per model group

## Screenshots

<img src="docs/images/manager.png" width="900" alt="QuantDock manager with model cards">

*The manager: model cards, Load, Monitor, and Logs.*

<img src="docs/images/overlay.png" width="600" alt="QuantDock overlay">

*The floating overlay: live GPU, CPU, RAM and LibreHardwareMonitor sensors.*

## Stack

Rust + [egui](https://github.com/emilk/egui) (`eframe`, OpenGL via `glow`). No Node, no
bundled webview — two small native binaries.

## Install

See [`INSTALL.md`](INSTALL.md). It starts with a prompt you can paste to a local agent
and covers the release download, models folder, llama-server, Hugging Face download, the
model TOML, the overlay, and LibreHardwareMonitor.

## Build

```sh
cargo build --release
```

Produces `target/release/quantdock.exe` (the manager) and `target/release/monitor.exe`
(the overlay). Both are GUI-subsystem binaries — no console window.

## Using it

Launch **`quantdock.exe`** and leave it open; the overlay starts from its **Monitor**
button.

- **Monitor** is neutral when the overlay is closed, green ("Monitor on") while it runs,
  red ("Close") when hovered, and closes it on click.
- **Logs** opens a console docked at the bottom of the main window. It shows the last 200
  lines of the current model's `llama-server.log`, keeps following the newest line unless
  you scroll up, and stays on the last loaded model after the server exits. It never
  clears or edits the file.
- **Model cards**: a yellow border while a model is loading, green while it is serving.
- **Settings** tab: overlay width, interval, opacity, font size, rows, profiles, fields,
  colours and thresholds.
- Body text, tables and the preview stay left-aligned at any window width; only action
  buttons sit on the right.

## Overlay configuration

Everything lives in `%APPDATA%\QuantDock\settings.toml`; the GUI writes it, and hand edits
are picked up within a second. A malformed file is never overwritten — the app shows the
parse error and offers a reset.

```toml
[monitor]
width = 320            # px; 0 = fit the widest row exactly
interval_ms = 1000
background_alpha = 190 # 0..255
corner_radius = 0
font_size = 13
show_labels = true
always_on_top = true
show_close = true
position = "top-right"
margin = 16
graph_samples = 60
title = ""             # optional label shown above the rows

[[monitor.rows]]
fields = ["model", "uptime", "llama_ram"]

[[monitor.rows]]
fields = ["gpu", "vram", "gpu_temp", "power"]

[[monitor.rows]]
fields = ["gpu_hotspot", "gpu_mem_temp", "gpu_shared", "fan"]

[[monitor.rows]]
fields = ["cpu", "cpu_peak", "cpu_hot", "cpu_temp", "cpu_power"]

[[monitor.rows]]
fields = ["ram", "board_temp"]

[thresholds.gpu_temp]
warn = 70
crit = 82

[monitor.field_style]
gpu = "bar"
cpu = "graph_text"
```

An empty row is a spacer. Field styles are `text`, `bar`, `graph` and `graph_text`; bars
need a natural scale (percentages and memory pairs), and text-only fields ignore them.

**Fields:** `model`, `alias`, `ctx`, `gpu`, `vram`, `vram_pct`, `vram_bus`, `gpu_temp`,
`fan`, `fan_rpm`, `power`, `clock`, `pstate`, `gpu_hotspot`, `gpu_mem_temp`, `gpu_shared`,
`cpu`, `cpu_peak`, `cpu_hot`, `cpu_temp`, `cpu_power`, `ram`, `ram_pct`, `swap`,
`llama_ram`, `disk_free`, `uptime`, `port`, `board_temp`.

The window position is remembered in `%APPDATA%\QuantDock\window.toml`; "Reset position"
in Settings deletes it.

## Hardware sensors (optional)

CPU temperature, CPU package power, motherboard temperature, GPU hot spot, GPU memory
junction and shared GPU memory come from **LibreHardwareMonitor**, which Windows does not
provide natively.

Settings -> **Hardware sensors**:

- **Download** fetches the pinned LibreHardwareMonitor release into
  `%APPDATA%\QuantDock\LibreHardwareMonitor` and pre-enables its web server.
- **Use this path** points at an existing `LibreHardwareMonitor.exe`.
- **Start it** launches it; Windows asks for administrator because it reads hardware
  directly. Decline and those fields simply show `--`.
- **Start LibreHardwareMonitor with QuantDock** does that launch automatically on the next
  start. It is off by default because each launch shows the UAC prompt; enable it only
  when you want sensors live without pressing **Start it**. If you cancel UAC, it is not
  retried until the next launch.
- Once connected, its fields become live. They are read from
  `http://127.0.0.1:8085/data.json` and matched by `HardwareId`/`SensorId`, so AMD and
  Intel machines both work.
- LibreHardwareMonitor escapes `°` as `\u00B0`; the parser accepts escaped values. The
  sampler reads LHM every `sensors.interval_ms` (default 2 s) but reuses the last good
  reading on every display tick. A failed refresh keeps the values for 10 s, then shows
  `--`.

## Layout convention (models)

```
<Model Name>/
  <model>.gguf
  <Model Name>.toml     <- per-model config (edit this, not a launcher)
```

The model root defaults to `C:\aiModels`; override with `QUANTDOCK_MODELS`.
`llama-server.exe` is found in the LM Studio backends directory (highest version wins);
override with `QUANTDOCK_LLAMA`.

## Aliases and OpenCode

| alias | OpenCode entry | models |
|---|---|---|
| `local` | Local (Qwen3.8-27B, 262K) | R3-mix, Q4_K_M, TurboFCFusion, MSVC9 Matcher |
| `local-3d` | Local (3D Coder, 262K) | Qwen3.5-4B-3D-Coder |
| `local-twil` | Local (TwIL-8B, 131K) | TwIL-8B-verbose |

## Memory

QuantDock's own heap is flat: a 10-minute run and a Rust heap counter show no retained
application allocations. The process commit still grows about 3 MB/min because the NVIDIA
OpenGL driver keeps GPU backing store; a xperf `VIRT_ALLOC` trace attributes it to
`nvoglv64` / `dxgmms2`, not to QuantDock. The churn fixes (zero-allocation RAM/swap read,
5 s process refresh, 3 s server-status cache, shared LibreHardwareMonitor reader, halved
repaint rates) cut the app's own allocations by 24-65%. This is accepted as a driver
floor, not a leak in the application.

## Tests

```sh
cargo test --all-targets
```

Four tests are ignored: they need a desktop, a live LibreHardwareMonitor on port 8085, or
a saved window position.

Covers the model config round-trip (comments preserved), the generated llama-server command
line, model discovery, the overlay config (defaults, migration, file safety), sampling
helpers, sensor parsing (including escaped `\u00B0` values), the sampler's LHM cache,
panel formatting, custom-field resolution, profile snapshots, thresholds, geometry, the
bounded server-log tail, and the `gpu_mem_temp` / autostart migrations.

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

## Saving settings

Settings edits a draft. **Save** writes `settings.toml`; **Return** discards the draft and
goes back to the model list. The tab shows `Settings *` while there are unsaved changes.
