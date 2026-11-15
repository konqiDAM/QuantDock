# QuantDock — log console, LibreHardwareMonitor autostart, and sensor polish

Date: 2026-11-15
Status: design, awaiting approval
Supersedes: the monitor-button, not-running-warning, overlay-autostart and slim-bar
parts of `2026-11-15-monitor-lifecycle-bar-logs.md`. The memory addendum in that file
remains the measurement record.

## Intent

QuantDock should show what llama-server is saying without leaving the app, start its
sensor backend on its own, repair older overlay configs, and let the user pick sensors
instead of pasting ids. No new windows, no new dependencies, no changes to the Monitor
button — it already behaves as wanted.

"Autostart" here means **LibreHardwareMonitor**, the tool QuantDock reads sensor data
from. It does not mean the overlay. The unused CPUID HWMonitor exe in the repo root is
not involved.

## Current state

| Area | What exists today | Where |
|---|---|---|
| Monitor button | Tri-state: `Monitor` neutral, `Monitor on` green, `Close` red on hover. Click opens or closes the overlay. Works; leave it alone. | `src/bin/quantdock.rs` |
| Overlay | Separate `monitor.exe`, single instance via `Local\QuantDockMonitor`, transparent, always-on-top. | `src/bin/monitor.rs` |
| Server log | `load()` truncates `<model folder>\llama-server.log`, writes stdout+stderr to it, and reads one error line only when startup fails. There is no viewer. | `src/server.rs` |
| LibreHardwareMonitor | Probe reports `Connected` / `InstalledNotRunning` / `Missing`; **Start it** uses `ShellExecuteW("runas")`; no automatic start. | `src/bin/quantdock.rs` |
| Custom sensors | Paste a `SensorId`; it is appended to the last row with a guessed label. No list, no values, no target-row choice. | `src/bin/quantdock.rs` |
| `gpu_mem_temp` | Present in the default rows; `migrate()` only renames `temp`→`gpu_temp` and `gpu_mem`→`vram_bus`. The live config has `gpu_hotspot, gpu_shared, fan` and no MEM. | `src/monitor_config.rs` |
| Settings drafts | Save/Return with a dirty marker; the watcher is skipped while dirty, so an external edit is silently ignored. | `src/bin/quantdock.rs` |
| Memory | Churn fixes shipped; live heap measured flat at 4.8 MB; commit still crept 8–9 MB/min. No 10-minute plateau run. | `docs/superpowers/specs/2026-11-15-monitor-lifecycle-bar-logs.md` |

## Goals

1. A **log console** docked at the bottom of the main window, toggled by a `Logs` button.
2. **LibreHardwareMonitor autostart** with QuantDock, opt-in per install.
3. **`gpu_mem_temp` migration** for configs that predate the field.
4. A **sensor picker** that lists live LHM sensors with values and adds one in a click.
5. A **missing-sensor marker** for stored custom ids that the machine no longer reports.
6. A **disk-change notice** when `settings.toml` changes while the draft is dirty.
7. A **10-minute plateau run** proving memory is bounded at current defaults.

## Non-goals

- Changing the Monitor button or adding an ON/OFF entry or warning.
- A slim right-edge bar. It was never requested; dropped.
- Overlay autostart. Replaced by LibreHardwareMonitor autostart.
- Piping llama-server output through a reader thread. The log file is the source.
- Clearing, truncating or rotating the log file.
- Showing QuantDock's own events in the console. The existing note line covers those.
- Per-machine sensor presets, or sharing layouts between machines.
- A picker window separate from Settings.

## Anti-goals

- Reading a large log into memory, or blocking the UI while the log grows.
- Repeated UAC prompts when autostart is cancelled.
- Duplicating a field during migration or reordering a user's rows.
- Remapping a missing custom sensor to a different sensor.
- Reintroducing per-tick full-document HTTP parsing on the UI thread.

## Constraints

- No new crates.
- All LHM HTTP work stays off the UI thread.
- Unknown field ids are preserved; existing configs keep loading.
- Settings stay draft-based: Save writes, Return discards.
- Windows-only, as today.
- The console is session state; it is not written to `settings.toml`.

## Approaches considered

**Log console.** (a) A bottom dock panel in the main window, above the status bar —
chosen: one window, resizable, matches "like a console". (b) A separate log window —
extra window and lifecycle for no gain. (c) Only "open in editor" — sends the user out
of the app exactly when a load failed.

**LHM autostart.** (a) The manager starts LHM with `runas` at launch — chosen: uses the
existing install and UAC path, no setup outside the app. (b) A Windows Scheduled Task
with highest privileges — no UAC, but requires an installer or manual setup. (c) Keep
manual only — the status quo the user asked to change.

**Sensor picker.** (a) An inline, searchable list inside Settings that fetches on open —
chosen: no new window, values visible, one fetch per open. (b) A modal picker — more
code and focus handling. (c) Keep paste-only — the spec's chosen picker was never built.

**Missing marker.** (a) Extend the existing 5 s probe to return the sensor ids it already
parses — chosen: no extra request. (b) Fetch LHM in Settings — duplicates the probe.
(c) No marker — leaves a silent `--`.

## Design

### 1. Log console

**Placement.** A `Logs` toggle button in the bottom status bar, left of `Unload`. When
on, `egui::Panel::bottom("logs")` is shown **after** the status bar panel, so it sits
directly above it. `resizable(true)`, `default_size(180.0)`, `min_size(80.0)`,
`max_size` = 50% of the window height. Off by default on every launch.

**Which log.** `server::log_path(model)` returns `<model folder>\llama-server.log`.
The manager tracks `last_log: Option<PathBuf>`:

- set when **Load** is clicked, before the server starts, so a failed load is readable;
- set when a serving model is detected at startup;
- kept after the server exits; replaced on the next load.

**Header.** `Logs — <model name>`, the log path in weak small text, and buttons:
`Refresh`, `Open in editor`, `Copy path`, `Close`. With no model ever loaded:
`No model loaded — load a model to see its server log`.

**Body.** Monospace, selectable text in a vertical `ScrollArea` with
`stick_to_bottom(true)`, so it follows the tail unless the user scrolls up. While open,
the tail is re-read at most once a second. The buffer is **replaced**, never appended:
`self.log_text = lines.join("\n")`.

**Reading.** `server::tail(path, max_lines)`:

- open the file; missing or unreadable returns an empty vector;
- seek to `len.saturating_sub(64 * 1024)` with `SeekFrom::Start`, then read to the end
  (a negative `SeekFrom::End` errors on files shorter than the window);
- split on lines and keep the last `max_lines` (200 for the console);
- if the read started mid-file, drop the first partial line;
- never read the whole file.

`load()` uses `log_path()` so there is one definition, and `last_error_line()` uses
`tail(path, 200)` instead of reading the whole file on a failed start.

**Failure behaviour.** A failed load writes the error line to the existing note. The
builder may also auto-open the console on failure; it is not required.

### 2. LibreHardwareMonitor autostart

**Config.** `[sensors] autostart: bool`, default **false**. Off because each launch costs
a UAC prompt; the user opts in. It appears in Settings → Hardware sensors as
`Start LibreHardwareMonitor with QuantDock`.

**Condition.** On the first poll where the probe state is known, start it once when all
hold:

- `sensors.enabled` and `sensors.autostart`;
- the state is `InstalledNotRunning`;
- at least one row field needs LHM: a built-in `field_needs_lhm(id)` or any `lhm:*` id.

`needs_lhm(&Config)` is a small pure function with a test. If LHM is already
`Connected`, do nothing. If no field needs it, do not start it.

**One shot.** `lhm_autostart_done: bool` is set after the first attempt, whatever the
outcome. Cancelling UAC produces one note, never a retry loop. It starts as true unless
sensors and autostart were both already on at launch, so enabling either one mid-session
never spawns LHM; `Start it` remains the manual path.

**Probe cleanup.** Today `poll()` calls `lhm_probe_state()` on the UI thread every 1.5 s
*and* a probe thread runs every 5 s; both fetch and parse the full `data.json`. The
synchronous call is removed. The 5 s thread becomes the only reader and returns
`{ state, ids }` (ids from the same parse) for the missing marker below.

### 3. `gpu_mem_temp` migration

After the existing renames, for every row: if it contains `gpu_hotspot` but not
`gpu_mem_temp`, insert `gpu_mem_temp` immediately after `gpu_hotspot`. Never insert a
duplicate, never move other fields, never touch rows without `gpu_hotspot`.

This repairs the live config's third row (`gpu_hotspot, gpu_shared, fan`).

### 4. Sensor picker

**Source.** `monitor_lhm::Reader::read()` already returns `LhmData.all`, a
`BTreeMap<lhm:<SensorId>, SensorValue { text, unit, value }>`. The picker flattens it to
`PickerSensor { id, text, unit, value }`.

**Fetch.** A `Browse sensors` button in Custom sensors. Every time it opens, or pressing
`Refresh`, spawns a thread with its own `Reader` and sends the vector back through a
channel; the UI shows a spinner while busy. Reopening therefore never shows a stale
list. No HTTP on the UI thread, no periodic picker fetch.

**List.** A search box filters case-insensitively on text and id. A flat list, sorted by
text then id, inside a 200 px scroll area. Each line shows the sensor text, its value and
unit, the id in weak small monospace, and an `Add` button. Duplicate texts remain
separate because the id is the identity.

**Target row.** A `Add to: Row N` combo, defaulting to the last row. `Add` inserts the
`lhm:*` id into that row if absent and inserts a label in `monitor.custom`: the LHM text
uppercased and truncated to 12 characters, editable afterwards. Adding the same id to
the same row is disabled. The paste box stays as a fallback for ids LHM is not currently
reporting.

**Empty states.** LHM not connected: `Start LibreHardwareMonitor to list sensors.` No
sensors after a fetch: `No sensors reported.` The existing paste box remains usable.

### 5. Missing-sensor marker

The 5 s probe returns the set of ids it saw. While the state is `Connected`, the Custom
sensors list shows a weak amber `(missing)` next to any stored `lhm:*` id not in that
set. While LHM is down the marker is not shown, so a disconnected tool does not mark
everything missing. The overlay keeps showing `--`; nothing is remapped.

### 6. Dirty draft vs. disk change

`poll()` calls `cfg_watcher.changed()` unconditionally. If it fires and the draft is
clean, reload as today. If it fires while dirty, set `disk_changed = true` and keep the
draft. Settings shows an amber line: `settings.toml changed on disk — Return reloads it,
Save overwrites it.` Save and Return clear the flag. Consuming the watcher stamp means
the change cannot later reload behind the user's back.

### 7. Memory plateau run

Not a feature; a verification task on the release build with the manager and overlay
running, sensors on, and current defaults (overlay 1 s, manager 1 s, sensors 2 s).

- Sample private commit and working set every 30 s for 10 minutes.
- Discard the first 2 minutes (startup warm-up).
- Pass: growth ≤ 2 MB/min and no monotonic climb across the final 5 minutes.
- Fail: take a Windows heap trace before changing code.

## File structure

| File | Change |
|---|---|
| `src/monitor_config.rs` | `SensorsSettings.autostart`; `gpu_mem_temp` sibling migration; tests. |
| `src/server.rs` | `log_path(model)`; `tail(path, max_lines)`; `load()` uses `log_path`; tail tests. |
| `src/bin/quantdock.rs` | Console state, panel and toggle; LHM autostart and one-shot guard; probe returns ids; picker state/UI/fetch channel; missing marker; `disk_changed` notice. |
| `README.md` | Console, LHM autostart, migration. |

`src/monitor_lhm.rs` needs no new API: `Reader`, `LhmData` and `SensorValue` are already
public.

## Tests

- `migration_adds_gpu_mem_temp_after_hotspot`
- `migration_does_not_duplicate_gpu_mem_temp`
- `migration_leaves_other_rows_alone`
- `tail_returns_last_lines`
- `tail_missing_file_is_empty`
- `tail_handles_short_file`
- `tail_drops_partial_first_line`
- `autostart_requires_enabled_and_an_lhm_field`
- `autostart_ignored_without_lhm_fields`
- `picker_add_does_not_duplicate_a_field`

## Proposed order

1. Migration, autostart config and condition, disk notice — small, mostly config.
2. Console: `log_path` + `tail` + tests, then the panel.
3. Picker and missing marker.
4. README, `cargo test --all-targets`, `cargo build --release`, 10-minute run, review,
   push, CI.

## What is left to the builder

- Console default height (180 px chosen) and whether a failed load auto-opens it.
- Picker grouping (flat list with search chosen; no hardware grouping).
- Label truncation length (12 characters chosen).
- Whether the picker reuses probe data (fetch on open chosen; simpler and fresher).
- Autostart default (off chosen; one UAC prompt per launch is the cost).

## Review focus

- The console never reads the whole log and never appends to its buffer.
- The console panel does not fight the status bar for the bottom edge.
- Picker fetches happen on a thread; the UI only receives a vector.
- Autostart fires once, only with a field that needs LHM, and never retries after UAC.
- Migration neither duplicates nor reorders.
- Unknown and missing ids survive save and reload.
- The disk notice consumes the watcher change, so Return does not resurrect stale values.
- No new per-tick HTTP parsing on the UI thread.
