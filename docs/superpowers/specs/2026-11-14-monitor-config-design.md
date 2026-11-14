# QuantDock — Configurable Monitor

Date: 2026-11-14
Status: design, awaiting approval

## Intent

Turn the floating monitor from a fixed five-line panel into a **configurable overlay**:
the user decides the update rate, the size and transparency, which metrics appear, and
what turns amber or red. Layout is the centre of it — the user wants to arrange metrics
into any number of rows, wide or square, instead of accepting a fixed stack.

All configuration lives in the **main QuantDock window**. The overlay stays a dumb
renderer that reads a config file.

## Goals

- Rows of fields: any number of rows, any subset of metrics in each, in any order.
- Window size follows the layout: fixed width, content-derived height, wrap when needed.
- Thresholds per metric: value turns amber (warn) or red (critical).
- Configurable: update interval, background transparency, corner radius, font size,
  always-on-top, close button, label visibility.
- Theme colours for normal / warn / critical / labels / accent / background.
- CPU display compressed for 44 logical cores: total %, peak core %, count of hot cores.
- RAM, swap, llama-server host RAM, free disk.
- Manager's Monitor button reflects state and doubles as the close control.

## Non-goals

- System-tray icon. The overlay has an `x`; a second launch raises the existing window.
- Historical graphs, sparklines, or logging.
- A per-core bar strip. 44 bars is noise; `cpu_hot` carries the same information.
- Multiple simultaneous overlays. One mutex-guarded instance.
- macOS/Linux. The overlay is Windows-only code.

## Anti-goals

- The overlay must never block its UI thread on a sample. Any change that reintroduces
  a synchronous `nvidia-smi` or HTTP call in `ui()` is a failure.
- The configuration must not require editing the `.gguf` folders or the model `.toml`s.
- Adding a field must not require a new row or change the others' layout.

## Constraints

- Windows only. `nvidia-smi` for GPU, `sysinfo` for CPU/RAM/disk.
- 44 logical cores (dual Xeon E5-2696 v4). Per-core values must be aggregated.
- `nvidia-smi` costs ~100 ms per call; `/props` and `/v1/models` are localhost HTTP.
- The overlay is a separate process; the manager edits files, not the overlay's memory.
- Display is 150 % scaled; all window sizing must go through physical pixels via the HWND.

## Approaches considered

The question was where configuration lives.

1. **Everything in the main window (chosen).** The manager owns the editor; the overlay
   reads `settings.toml` and hot-reloads. One place to look, room for a row builder, and
   the overlay binary stays small. Cost: changing the overlay means switching windows.
2. **Gear icon in the overlay.** Adjust while watching live, but a borderless 320 px
   always-on-top window has no room for a row builder, and it bloats the renderer.
3. **Separate settings executable.** Rejected: a third binary for no benefit.

For the editor itself, three shapes were considered:

1. **Full form** — a control per field, threshold and colour. Most discoverable, most
   code, cramped for arbitrary row layouts.
2. **Row builder (chosen)** — rows are lists of field chips with add/remove/reorder, plus
   steppers and colour pickers for the global settings. Visual, bounded, and it matches
   the thing the user actually tunes: the arrangement.
3. **Text editor with live preview** — complete power, least code, requires knowing field
   names. Kept as an eventual escape hatch (the file is plain TOML), not the primary UI.

## Design

### Config file

`%APPDATA%\QuantDock\settings.toml`, created with defaults on first run. The manager
writes it (debounced ~300 ms after a change); the overlay watches its mtime once a second
and reloads. A malformed file is ignored in favour of the last good config, with the error
surfaced in the manager.

```toml
[monitor]
width = 320            # logical px; 0 = fit the widest row
interval_ms = 1000     # sample + repaint cadence, clamped 250..10000
background_alpha = 190 # 0..255
corner_radius = 12     # logical px
font_size = 13         # logical px
show_labels = true     # "GPU 15%" vs "15%"
always_on_top = true
show_close = true
position = "top-right" # top-right | top-left | bottom-right | bottom-left
margin = 16            # logical px from the screen edge

# A row is an ordered list of fields, rendered left to right.
# An empty list renders a small vertical gap, for grouping.
[[monitor.rows]]
fields = ["model"]

[[monitor.rows]]
fields = ["gpu", "vram"]

[[monitor.rows]]
fields = ["cpu", "cpu_peak", "cpu_hot"]

[[monitor.rows]]
fields = ["temp", "fan", "power"]

# Thresholds are opt-in per field. Default direction is "above".
[thresholds.temp]
warn = 70
crit = 82

[thresholds.gpu]
warn = 90
crit = 98

[thresholds.cpu_peak]
warn = 80
crit = 95

[thresholds.vram_pct]
warn = 90
crit = 97

[thresholds.disk_free]
warn = 100
crit = 40
direction = "below"

# Percentage fields can render as a bar instead of text. Default is "text".
[monitor.field_style]
gpu = "bar"
vram = "bar"

[theme]
text = "#d6d6d6"
label = "#8a8a8a"
accent = "#78beff"
warn = "#e0b050"
crit = "#e07878"
background = "#000000"
```

### Field vocabulary

Every field formats as `<label> <value>` (label omitted when `show_labels = false`).
An unavailable value renders `--` in the label colour and never triggers a threshold.

| id | label | value | source |
|---|---|---|---|
| `model` | — | model folder name | `/props` → `model_path` |
| `alias` | — | served model id | `/v1/models` |
| `ctx` | `ctx` | context size, e.g. `262K` | `/props` |
| `gpu` | `GPU` | `15%` | `utilization.gpu` |
| `vram` | `VRAM` | `3.2/24.0 GB` | `memory.used` / `memory.total` |
| `vram_pct` | `VRAM` | `13%` | derived |
| `gpu_mem` | `VRAM IO` | `1%` | `utilization.memory` |
| `temp` | — | `52 °C` | `temperature.gpu` |
| `fan` | `fan` | `61%` | `fan.speed` |
| `power` | — | `112 W` | `power.draw` |
| `clock` | — | `225 MHz` | `clocks.sm` |
| `pstate` | — | `P0` | `pstate` |
| `cpu` | `CPU` | `34%` | sysinfo global CPU |
| `cpu_peak` | `peak` | `92%` | max over logical cores |
| `cpu_hot` | `hot` | `3` | cores at or above `cpu_peak.warn` |
| `ram` | `RAM` | `12.4/64.0 GB` | sysinfo |
| `ram_pct` | `RAM` | `19%` | derived |
| `swap` | `swap` | `0.0/8.0 GB` | sysinfo |
| `llama_ram` | `llama` | `812 MB` | llama-server process RSS |
| `disk_free` | `disk` | `431 GB` | free space on the model root's volume |
| `uptime` | — | `01:24` | llama-server process start time |
| `port` | `port` | `8080` | constant |

`cpu_hot` reuses `thresholds.cpu_peak.warn` so one number drives both fields.

### Rendering rules

1. Rows are laid out top to bottom. Each row is a horizontal strip of fields separated by
   `item_spacing.x = 12`.
2. `width = 0` sizes the window to the widest row. A fixed `width` wraps an over-long row
   onto the next line rather than clipping it.
3. The window height is always content-derived; there is never dead space. After layout the
   renderer compares the used size with the current window size and sends a resize when
   they differ by more than 1 px.
4. The background is one rounded rect painted over the whole window; the drag area and the
   close button sit on top of it.
5. Only a field's value carries threshold colour; its label stays in the label colour.
6. Numbers are formatted without decimals below 100 and with one decimal above, except
   memory, which always uses one decimal.
7. A field whose style is `bar` draws a filled bar sized to the field's own share of the
   row, with its text drawn over or beside it. Bars are inset from the panel's right margin
   like every other field. Only percentage fields accept the `bar` style: `gpu`,
   `gpu_mem`, `vram_pct`, `cpu`, `ram_pct`, `fan`.

### Config ownership and conflicts

The manager owns the file; the overlay only reads it.

- The manager keeps an in-memory config, writes it ~300 ms after any change, and **watches
  the mtime**. If the file changes underneath it (hand edit), it reloads and rebuilds the
  row builder from the file rather than silently overwriting the edit on the next slider
  move.
- If the file is malformed, the manager does **not** write over it. It shows the parse
  error and a single "Reset to defaults" action, so a typo cannot destroy the config.
- Saves regenerate the whole file from the UI state, so comments in it do not survive a UI
  edit. The file starts with a generated header saying so. Hand editing remains viable for
  setting values the UI does not expose, but the UI is the primary path.

### Positioning

`position` and `margin` are applied **once at startup**, on the primary monitor, and never
re-applied on hot reload — otherwise changing the font size would snap a dragged window
back to the corner. Dragging is session-only: the overlay never writes the config.

### Preview data

The settings preview renders from a **fixed, realistic sample** embedded in the manager,
not from live sampling:

```
Qwen3.8-27B-R3-mix   [local]   GPU 15%   VRAM 3.2/24.0 GB
CPU 34%   peak 92%   hot 3     52 °C   fan 61%   112 W
```

The preview exists to judge arrangement, width and colour, all of which a fixed snapshot
shows. Giving the manager its own sampler would double `nvidia-smi` traffic and make the
preview refresh at whatever `interval_ms` says, which may be ten seconds. The one field
above a threshold in the sample is tinted, so warn/crit colours are visible without waiting
for hardware to misbehave.

### Sampling

One background thread owns all data collection and sends a `Sample` snapshot over a
channel. The UI thread only ever drains that channel.

- The thread reads the live config through an `Arc<RwLock<Config>>` and **only samples what
  is displayed**: no `nvidia-smi` call when no GPU field is present, no HTTP when no
  model/server field is present.
- Order per tick: `nvidia-smi` (one call for every GPU field) → sysinfo → HTTP.
- sysinfo needs two CPU samples at least 200 ms apart; the thread keeps its `System`
  instance across ticks, so any `interval_ms` ≥ 250 is valid.
- `cpu_peak` and `cpu_hot` are computed from the 44 logical cores once per tick.
- A failed source leaves its fields `None`; it never aborts the tick.

### Manager: Settings UI (row builder)

A **Settings** section in the main window, next to the model list.

- **Rows panel.** A vertical list of rows. Each row shows its fields as chips in order.
  Per row: `+ field` (dropdown of the vocabulary, already-used fields disabled), a remove
  `x` on each chip, move-left / move-right on each chip, and delete-row. Between rows:
  add-row, move-up / move-down.
- **Layout controls.** Width (`0` = auto), interval, background alpha, corner radius,
  font size, margin, show labels, always on top, show close, position corner.
- **Field table.** One line per field in use: display style (`text` / `bar`, only offered
  for percentage fields), warn, crit, direction. Fields with no threshold are shown greyed
  so it is obvious what can be tuned.
- **Theme.** Six colour swatches with a picker.
- **Preview.** The overlay's own row renderer, rendered inside the settings panel at the
  configured width and font size, so "wide or square" is tuned by looking at it. Same code
  path as the overlay — no separate implementation — fed by the fixed sample above.
- Every change writes the config and updates the preview immediately; the overlay picks it
  up within a second.

### Manager: Monitor button

Replaces today's plain button. State is detected by opening the monitor's named mutex
(`Local\QuantDockMonitor`) without creating it.

| overlay | button | hover | click |
|---|---|---|---|
| not running | neutral, "Monitor" | neutral | spawn |
| running | green fill, "Monitor on", dot | red fill, "Close" | post `WM_CLOSE` to `QuantDock Monitor` |

Colours are named constants for now; they become theme entries later.

### Shared code layout

```
src/monitor_config.rs   Config, Field, Row, thresholds, load/save, defaults, hex colours
src/monitor_sample.rs   Sample struct, sampler thread, per-source collection
src/monitor_panel.rs    render(ui, &Config, &Sample) — used by the overlay and the preview
```

`src/bin/monitor.rs` becomes: load config, own the sampler, call `render`.
`src/bin/quantdock.rs` gains the settings section and the monitor button states.

### Bugs to fix as part of this work

1. **Red box behind the close button.** egui's ID-clash marker appears when the overlay has
   focus: two widgets claim the same `Id` at different rects. Give the close button an
   explicit, stable `Id` (for example via `ui.push_id`) and confirm no marker appears while
   focused and while dragging.
2. **Bars touch the right edge.** The current bars overrun the panel's right inner margin.
   Every field, bar or text, is laid out inside the margin, and the bar width comes from
   the space measured inside it. Verify a visible gap on both sides at 100 % and 150 %
   scaling, with the bar at 0 % and at 100 %.

## Milestones

1. Config model, defaults, load/save, mtime watching, colour parsing, generated header.
2. Sampler: sysinfo CPU/RAM/disk/process + extended `nvidia-smi` query, gated by the fields
   in use.
3. Shared renderer: rows, wrapping, auto-size, text and bar field styles, thresholds.
4. Settings UI: row builder, field table (style + thresholds), theme, preview.
5. Monitor button states and close via `WM_CLOSE`.
6. The two bug fixes and a pass at 100 % / 150 % scaling.

## Left to the builder

- Exact chip interaction details (drag to reorder vs arrow buttons; arrows are the minimum).
- Whether the preview is scaled down when the panel is narrower than the configured width.
- Formatting of `ctx` (raw number vs `262K`) and of `uptime`.
- How the settings section is laid out relative to the model list (tabs, split, or a
  collapsible section).
- Whether `interval_ms` below ~500 is worth allowing given `nvidia-smi` costs ~100 ms.
