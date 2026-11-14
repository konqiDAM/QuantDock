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

Settings → **Hardware sensors**:

- **Download** fetches the pinned LibreHardwareMonitor release into
  `%APPDATA%\QuantDock\LibreHardwareMonitor` and pre-enables its web server.
- **Use this path** points at an existing `LibreHardwareMonitor.exe`.
- **Start it** launches it; Windows asks for administrator because it reads hardware
  directly. Decline and those fields simply show `--`.
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

## Tests

```sh
cargo test
```

Covers the model config round-trip (comments preserved), the generated llama-server command
line, model discovery, the overlay config (defaults, migration, file safety), sampling
helpers, sensor parsing, panel formatting, thresholds and geometry.
### Custom sensors

Any numeric sensor LibreHardwareMonitor reports can become a field: paste its
`SensorId` (for example `/nvidiagpu/0/clock/1`) under Settings ? **Custom sensors**, give
it a short label, and it is appended to the last row. It supports thresholds, and it
shows `--` when the sensor is not present. Config form:

```toml
[monitor.custom]
"lhm:/nvidiagpu/0/clock/1" = "GPU CLK"
```

### Saving settings

Settings edits a draft. **Save** writes `settings.toml`; **Return** discards the draft and
goes back to the model list. The tab shows `Settings *` while there are unsaved changes.