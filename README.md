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

## Stack

Rust + [egui](https://github.com/emilk/egui) (`eframe`, OpenGL via `glow`). No Node, no
bundled webview — two small native binaries.

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
- **Settings** tab: overlay width, interval, opacity, font size, rows, colours, thresholds.

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
repaint rates) cut the app's own allocations by 30-65%. This is accepted as a driver
floor, not a leak in the application.

## Tests

```sh
cargo test
```

Covers the model config round-trip (comments preserved), the generated llama-server command
line, model discovery, the overlay config (defaults, migration, file safety), sampling
helpers, sensor parsing, panel formatting, thresholds, geometry, the bounded server-log
tail, and the `gpu_mem_temp` / autostart migrations.

### Custom sensors

Any numeric sensor LibreHardwareMonitor reports can become a field. Under Settings ->
**Custom sensors**, press **Browse sensors** for a searchable list of every live sensor
with its value and unit; pick a target row and press **Add**. The label is a short
uppercase version of the sensor name and stays editable. A stored sensor the machine no
longer reports is marked `(missing)` and shows `--` in the overlay; it is never remapped.

The paste box remains as a fallback for a `SensorId` (for example
`/nvidiagpu/0/clock/1`) that LibreHardwareMonitor is not currently reporting. Config form:

```toml
[monitor.custom]
"lhm:/nvidiagpu/0/clock/1" = "GPU CLK"
```

### Saving settings

Settings edits a draft. **Save** writes `settings.toml`; **Return** discards the draft and
goes back to the model list. The tab shows `Settings *` while there are unsaved changes.
