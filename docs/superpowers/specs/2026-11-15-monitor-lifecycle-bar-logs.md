# QuantDock — monitor lifecycle, slim bar, and llama logs

Date: 2026-11-15
Status: superseded in part (2026-11-15)
Superseded by: `2026-11-15-log-console-sensor-polish-design.md`

The Monitor button, the not-running warning, overlay autostart and the slim right-edge
bar are dropped. The button works as wanted and the bar was never requested. This file's
memory addendum remains the measurement record.

## Intent

QuantDock becomes the single place where the overlay's lifecycle and the server's output
are visible: it starts the overlay automatically if asked, says plainly whether the overlay
is up, warns when it is not, can shrink to a slim always-on-top strip at the right edge of
the screen, and can show the llama-server's log without leaving the app.

## Goals

- Restore `gpu_mem_temp` for users whose saved rows predate it.
- A visible **overlay ON/OFF** entry with two buttons: open and stop.
- A clear warning in the app when the overlay is not running.
- A setting to **auto-open the overlay** when the manager starts.
- **Minimize to a slim bar** on the right edge; the bar can bring the main window back.
- A **Logs** button at the bottom showing the llama-server's output for the current model.

## Non-goals

- A Windows system-tray icon. The slim bar is the always-present surface.
- Editing the llama-server's flags from the app (the model `.toml` already does that).
- Multiple overlays, or moving the bar between monitors.
- Live streaming of logs into the model list; a tail of the file is enough.

## Design

### 1. Restoring `gpu_mem_temp`

The default rows already include it; the user's file was written before the field existed.
A migration adds a newly-introduced sensor field to the row that already carries its
siblings: if a row contains `gpu_hotspot` but not `gpu_mem_temp`, add `gpu_mem_temp` right
after it. Unknown ids are never removed.

### 2. Overlay status, buttons and warning

- The manager already knows the overlay's state through the named mutex
  (`Local\QuantDockMonitor`); `monitor_running()` is reused.
- The top bar gains an entry: `Overlay: ON` (green dot) or `Overlay: OFF` (amber dot),
  with **Open** and **Stop** buttons. Open spawns `monitor.exe`; Stop posts `WM_CLOSE` to
  the `QuantDock Monitor` window.
- While the overlay is off, the bottom status bar shows a warning line:
  `Overlay is not running` with a click-to-open hint. It is a warning, not a modal.

### 3. Auto-open

`[monitor] autostart = true` in `settings.toml`. On manager start, if the setting is on and
the overlay is not already running, spawn it. Default **false** so nobody is surprised by
a second window appearing.

### 4. Slim bar (minimize to the right edge)

Implemented as a second native window using egui's viewport support
(`Context::show_viewport_deferred`), which creates a real OS window on Windows.

- Geometry: 34 logical px wide, 240 tall, pinned to the right edge of the primary
  work area with a 16 px margin, always-on-top, no decorations, transparent.
- Content: the app's initial, the server state, the overlay state, and a small
  `«` button that restores the main window.
- The main window gets a **Minimize to bar** button (bottom bar). The main window then
  hides; the bar appears. **Show app** on the bar reverses it.
- The bar is a viewport of the same process, so there is no IPC and no second binary.
- The bar never takes focus on hover; clicking it does.

### 5. Llama logs

`server::load` already redirects the server's stdout and stderr to
`<model folder>\llama-server.log`. The manager gains a **Logs** button in the bottom bar
that opens a collapsible panel showing the last ~200 lines of that file, refreshed every
second while open, with **Refresh**, **Open in editor** (`ShellExecute` "open" on the file)
and **Copy path**. The panel shows which model's log it is and says so when the server is
not running.

### Approaches considered

- **Second binary for the bar** — no: a viewport in the same process is simpler and shares
  state with the main window.
- **System tray icon** — the classic solution, but the user asked for a visible bar at the
  right, and a tray icon needs an icon asset and a message pump.
- **Spawning llama-server from the `.bat`** — rejected: the app already spawns
  `llama-server.exe` directly with the exact same arguments, and a `.bat` would give up
  the log file and the error detection we have. The `.bat` files stay as a manual fallback.
- **Piping the server's output into the app** — rejected for now: the log file already
  exists, and a pipe needs a reader thread and a ring buffer. The file tail is enough.

## Constraints

- The overlay must still be a single instance (named mutex unchanged).
- The bar must not fight the always-on-top behaviour we already fought with OpenCode.
- Logs are a file on disk; the panel never blocks the UI thread on a big read (tail only).
- Settings stay draft-based with Save / Return.

## Review focus

- The overlay is off: the warning must be visible but must not nag over other work.
- The bar must not be left on screen after the app exits (viewport dies with the process).
- Autostart must not spawn a second overlay when one is already running.
- The logs panel must not read the whole file on a 200 MB log.
- Restoring `gpu_mem_temp` must not duplicate a field the user already has.

## Left to the builder

- Exact bar contents (initials, dots, a mini sparkline).
- Whether the bar is draggable or fixed to the right edge.
- Log panel: plain text vs. a table of the last N lines with timestamps.

---

# Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or
> superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Overlay lifecycle visible and controllable from the manager, a slim right-edge bar
mode, and a llama-server log panel.

**Architecture:** `monitor_config` gains `autostart`; `server` already writes the log file;
the manager gains an overlay-status entry, a log panel, and a second egui viewport used as
the slim bar. The overlay binary is untouched except for accepting `WM_CLOSE`.

**Tech Stack:** Rust 1.100, eframe/egui 0.37, `windows` 0.62, `sysinfo` 0.40.

**Spec:** this document.

## Global constraints

- The overlay stays a single instance; `Open` must check before spawning.
- No UI-thread blocking: the log panel tails the file, the overlay check is a mutex open.
- Draft-based settings still apply: new settings are draft fields until Save.
- Windows only. All new child processes use `quantdock::hidden` or `ShellExecuteW`.
- 48 tests currently pass; they must stay green.

## File structure

```
src/monitor_config.rs   autostart setting; migration for gpu_mem_temp
src/server.rs            log path helper (already writes the file) + tail reader
src/bin/quantdock.rs     status entry, warning, logs panel, slim bar viewport
```

## Task 1 — Restore `gpu_mem_temp` and add `autostart`

- Files: `src/monitor_config.rs`; tests inline.
- Interfaces: `MonitorSettings.autostart: bool` (default `false`);
  `Config::parse` gains the sibling-field migration.
- Steps:
  1. Failing test: a config with `fields = ["gpu_hotspot"]` parses to a row containing
     both `gpu_hotspot` and `gpu_mem_temp`; a config already containing `gpu_mem_temp`
     does not gain a duplicate.
  2. `cargo test monitor_config` → fail.
  3. Implement: after `migrate(&mut cfg)`, for each row, if it has `gpu_hotspot` and not
     `gpu_mem_temp`, insert `gpu_mem_temp` directly after it. Add `autostart` to the struct
     and defaults.
  4. `cargo test` → green.
  5. Commit `feat: restore gpu_mem_temp for older configs and add autostart`.

## Task 2 — Overlay status entry, buttons and warning

- Files: `src/bin/quantdock.rs`.
- Interfaces: reuse `monitor_running()`; add `fn open_monitor()`, `fn close_monitor()`
  (reuse the `WM_CLOSE` code path), and `fn spawn_monitor()`.
- Steps:
  1. Top bar: `Overlay: ON/OFF` with the coloured dot, plus **Open** and **Stop** buttons.
  2. Bottom bar: when the overlay is off, show `Overlay is not running` in the warn colour.
  3. Clicking **Open** spawns `monitor.exe` and reports the outcome through the existing
     note channel; **Stop** posts `WM_CLOSE`.
  4. Manual: with the overlay closed, the entry reads OFF and the warning shows; Open
     starts it and the entry flips to ON within a second.
  5. Commit `feat: overlay status, open/stop buttons and a not-running warning`.

## Task 3 — Auto-open the overlay

- Files: `src/bin/quantdock.rs`.
- Steps:
  1. On startup, if `cfg.monitor.autostart` and the overlay is not running, spawn it.
  2. Never spawn when one is already running (mutex check).
  3. Settings: a checkbox bound to the draft field, in the Overlay grid.
  4. Manual: enable it, restart the manager, and confirm exactly one overlay appears.
  5. Commit `feat: optionally start the overlay with the manager`.

## Task 4 — Slim right-edge bar

- Files: `src/bin/quantdock.rs`.
- Interfaces: `fn bar_ui(&mut self, ui: &mut egui::Ui)`; a `bar: bool` state.
- Steps:
  1. Add a **Minimize to bar** button to the bottom bar; it hides the main window and
     shows a viewport: `ViewportBuilder::default().with_inner_size([34, 240])`
     `.with_decorations(false).with_always_on_top().with_transparent(true)`,
     positioned at the work area's right edge.
  2. Bar content: app initial, server dot, overlay dot, and a `«` button that restores the
     main window and hides the bar.
  3. The bar viewport is closed when the main window is restored or the app exits.
  4. Manual: minimize to the bar, restore from it, and confirm no bar is left after exit.
  5. Commit `feat: minimize the app to a slim bar on the right edge`.

## Task 5 — Llama log panel

- Files: `src/server.rs` (`pub fn log_path(model) -> PathBuf`, `pub fn tail(path, lines) ->
  Vec<String>`), `src/bin/quantdock.rs`.
- Steps:
  1. Failing test for `tail`: given a file with 500 lines, `tail(path, 200)` returns the
     last 200 lines, and a missing file returns an empty vector.
  2. `cargo test server` → fail.
  3. Implement `tail` by reading the last 64 KB with `Seek` from the end, then taking the
     last N lines. A bottom bar button **Logs** toggles a panel showing them, refreshed
     once a second while open, with **Refresh**, **Open in editor** and **Copy path**.
  4. Manual: start a model, trigger a load failure, and confirm the error line appears in
     the panel.
  5. Commit `feat: llama-server log panel in the manager`.

## Task 6 — Docs, review, push

- Files: `README.md`.
- Steps:
  1. README: overlay status entry, autostart, the slim bar, the logs panel.
  2. `cargo test` and `cargo build --release` clean.
  3. Fresh code review of the range; fix Critical and Important with tests.
  4. Commit and push; confirm CI green.

---

# Addendum — memory growth

Date: 2026-11-15. Reported as "memory keeps growing". Measured on this machine.

## Measurements

Two `quantdock` instances and the `monitor` overlay, sampled every 15 s for two minutes:

| process | private MB | growth | handles | threads |
|---|---|---|---|---|
| quantdock (older build) | 229 → 249 | +10 MB/min | 352, flat | flat |
| quantdock (current) | 258 → 281 | +11.5 MB/min | 350, flat | flat |
| monitor | 297 → 319 | +11 MB/min | 550, flat | flat |

`llama-server` is flat. Handles and threads are flat in all three, so this is **heap growth,
not a resource leak**. The rate is the same in three processes doing different work, which
points at the shared request path rather than at any one feature.

## Suspects, ranked

1. **A fresh `ureq::Agent` per request.** `monitor_lhm::fetch_data()` builds a new agent on
   every call (once a second in the overlay, once a five seconds in the manager). Each agent
   owns a connection pool with its own buffers, and the churn is pure waste.
2. **Full-body JSON parsing per request.** `/props` returns llama.cpp's whole property
   document (chat template included); LHM's `data.json` is 88 KB and is parsed into a
   `serde_json::Value` tree of ~310 nodes, every second, in both apps.
3. **Per-frame text measurement.** `natural_width` calls `layout_no_wrap` for every field on
   every frame, shaping text that is thrown away immediately. That is a few galleys per
   frame at 4 Hz in each app.
4. **Heap fragmentation / driver-side growth.** If the above are churn rather than
   retention, the Windows heap and the GPU driver may simply be ratcheting the high-water
   mark. This is the fallback hypothesis and needs a heap trace to confirm.

There is no unbounded container in the app: the history buffers are capped, the sample maps
are replaced each tick, the channels are drained, and the draft maps are bounded.

## Plan

### Task 7 — Instrument

- Files: `src/bin/quantdock.rs`, `src/bin/monitor.rs`.
- Steps:
  1. A debug line (behind a `?debug` style toggle in the status bar) showing the process's
     private bytes, `ctx.memory()` counters (atlas size, texture count, data-map length) and
     the sample's size, sampled once a second.
  2. Run each app for ten minutes with the overlay both running and stopped, and with
     sensors enabled and disabled, to see which input the growth follows.
  3. If the growth follows the request path, the fixes below are the answer; if it follows
     frame rate alone, take a Windows heap trace (Performance Recorder) and look at the
     largest retained allocations.

### Task 8 — Remove the churn

- Files: `src/monitor_lhm.rs`, `src/server.rs`, `src/monitor_panel.rs`.
- Steps:
  1. One shared `ureq::Agent` per process (a `OnceLock`), used by every request, instead of
     one per call.
  2. Read bodies into a reusable buffer where the size allows, and avoid `serde_json::Value`
     for `/props`: deserialise into a small struct that only carries `model_path` and
     `default_generation_settings.n_ctx`.
  3. Cache the measured text widths in the panel: they only change when the config or the
     displayed text changes, so `layout_no_wrap` does not run every frame.
  4. Fetch LHM no more often than the configured interval, and only when a sensor field is
     actually visible (already the case).
  5. Re-measure with the same ten-minute harness; the growth must be flat or bounded.

### Task 9 — Review

- Fresh code review of the whole range; fix Critical and Important with tests; push and
  confirm CI green.


---

## State at compaction (2026-11-15)

Shipped on main, pushed, CI green:

- LibreHardwareMonitor integration: optional, setup flow (Download / Use this path /
  Cancel), headless (start minimized to tray), sensors matched by HardwareId.
- Custom sensors: `lhm:<SensorId>` fields with labels, thresholds, `--` when absent.
- Settings are draft-based with Save and Return.
- GPU family fix: LHM 0.9.6 reports `/gpu-nvidia/0` (713f932).
- Memory churn fixes: `/props` cached 10 s, one shared ureq agent, LHM JSON parsed
  into borrowed structs, a reusable Reader (one buffer, one map), configurable
  `sensors.interval_ms` (default 2000), auto width estimated instead of shaped,
  repaint rates halved.
- Working-set trim removed (it only made Task Manager bounce) - 8c79754.
- Temporary leak probes and heap counter removed - 8266e2e.

Still open:

- Tasks 1-6 below are NOT implemented yet: gpu_mem_temp migration, overlay ON/OFF
  entry with Open/Stop, not-running warning, autostart, slim right-edge bar, llama
  log panel.
- Memory: commit still creeps about 8-9 MB/min. The live Rust heap is flat
  (measured 4.8 MB constant over 75 s), so this is allocator/OS churn, not
  retention. Slowing polling 10x plateaued the overlay at 84 MB, so the remainder
  follows the per-tick HTTP/render path. Next diagnostic: a 10-minute plateau run
  at the new defaults, then a Windows heap trace if it still climbs.
