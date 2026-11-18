# AGENTS.md

QuantDock is a Windows desktop manager for local GGUF models running under
`llama-server` (llama.cpp), plus a separate always-on-top resource overlay. It is Rust
2024 with egui/eframe 0.37 and OpenGL. There is no Node, Tauri, webview or installer.

This file is the onboarding map. Read it first, then the current spec and the code it
names. Keep it current when you change behavior.

## Start here

1. `README.md` — user-facing behavior, config, fields, aliases, the agent install
   prompt and the short manual install steps.
2. `INSTALL.md` — agent-facing install procedure (release download, models folder,
   llama-server, HF download, model TOML, overlay, LHM, OpenCode). Humans use
   README's manual steps instead.
3. `docs/superpowers/specs/2026-11-17-tray-hf-download-centered-settings-design.md` —
   current feature spec: centered settings column, HuggingFace download, tray icon.
   Its plan is `docs/superpowers/plans/2026-11-17-tray-hf-download-centered-settings.md`.
4. `docs/superpowers/specs/2026-11-15-sensor-tree-design.md` — the hardware-grouped
   sensor tree (shipped).
5. `docs/superpowers/plans/2026-11-15-sensor-tree.md` — that implementation plan; the
   commits on `main` are the completed result.
6. `docs/superpowers/specs/2026-11-15-sensor-window-and-profiles-design.md` and
   `docs/superpowers/plans/2026-11-15-sensor-window-and-profiles.md` — background for the
   sensor window, custom fields, profiles and left alignment. The log-console spec
   remains background.
7. `docs/superpowers/plans/2026-11-15-memory-churn-reduction.md` — the memory work, the
   measured plateau, and the VIRT_ALLOC driver finding.
8. The code map below.

`docs/superpowers/specs/2026-11-15-monitor-lifecycle-bar-logs.md` is **partly
superseded**. Its Monitor-button, warning, overlay-autostart and slim-bar sections are
dropped. Do not implement them from that file.

## Commands

```sh
cargo test --all-targets      # 132 tests: 103 lib + 29 bin, 4 ignored
cargo build --release         # target/release/quantdock.exe + monitor.exe
```

Launch `target\release\quantdock.exe`; the overlay starts from its **Monitor** button or
the tray menu. The 4 ignored tests need port 8085, the real `%APPDATA%`, a live
LibreHardwareMonitor, or a real `nvidia-smi` sample.

CI is `.github/workflows/build.yml`: every push and PR on `windows-latest` runs
`cargo test --all-targets`, `cargo build --release`, and uploads both exes as
`quantdock-windows`. `.github/workflows/release.yml` publishes a GitHub Release with both
exes when a `v*` tag is pushed. Main is `main`; docs and code ship together.

## Code map

| File | Responsibility |
|---|---|
| `src/bin/quantdock.rs` | Manager UI: model cards, Settings draft, sensor manager window (hardware tree), profiles, field tables, themes, preview, LHM setup, log console, HF download dialog, tray icon and the close/exit state machine. Large file; most UI lives here. |
| `src/bin/monitor.rs` | Overlay binary: transparent always-on-top window, single instance via `Local\QuantDockMonitor`, drag/resize, placement, per-frame render. |
| `src/lib.rs` | Shared modules, `hidden()` (CREATE_NO_WINDOW child processes). |
| `src/model.rs` | Model discovery, per-model TOML, `PORT = 8080`, model root. |
| `src/server.rs` | llama-server spawn/kill, `status()` cache, `/props` cache, `llama-server.log` tail. |
| `src/monitor_config.rs` | `Config`, `SensorsSettings`, rows/styles/thresholds/theme, profile slots, migrations, `settings.toml`/`window.toml` paths, watcher, atomic save. |
| `src/monitor_sample.rs` | Overlay sampler thread: sysinfo CPU/processes, `sysmem` RAM/swap, GPU via `nvidia-smi`, LHM values + cache, history. |
| `src/monitor_panel.rs` | Overlay renderer: FieldView resolver, rows, text/bar/graph styles, thresholds, auto width. |
| `src/monitor_lhm.rs` | LibreHardwareMonitor HTTP client, `data.json` parser, reusable `Reader`, picker-only hardware tree. |
| `src/hf.rs` | HuggingFace repo parsing, GGUF listing, streaming download with cancel/cleanup, TOML generation, partial sweep. |
| `src/sysmem.rs` | `GlobalMemoryStatusEx` RAM/swap read into a stack struct. |
| `src/gpu.rs` | `nvidia-smi` query. |
| `INSTALL.md` | Agent-facing install procedure, model downloads and per-model setup. |
| `docs/images/` | Screenshots used by `README.md`. |
| `.github/workflows/` | `build.yml` for every push/PR; `release.yml` for `v*` tags. |
| `docs/superpowers/` | Specs and plans. Read the current ones above. |
| `C:\aiModels\README.md` | Model catalogue, aliases and per-model notes (outside this repo). |

## Runtime paths and ports

- Config: `%APPDATA%\QuantDock\settings.toml` (atomic save; malformed file is never
  overwritten). Window position: `%APPDATA%\QuantDock\window.toml`.
- Models: `C:\aiModels` by default; override with `QUANTDOCK_MODELS`. Each model is a
  folder with one `.gguf`, a matching `.toml`, and a generated `llama-server.log`. The
  app only discovers a folder when `<folder>\<folder>.toml` exists and parses; `old` is
  skipped.
- llama-server: found in `%USERPROFILE%\.lmstudio\extensions\backends\llama.cpp-*`
  (highest version wins) or `QUANTDOCK_LLAMA`; listens on `127.0.0.1:8080`. Only one
  model runs at a time; `--port` uses a double dash.
- LibreHardwareMonitor: optional, administrator-only, `http://127.0.0.1:8085/data.json`.
  Download lives in `%APPDATA%\QuantDock\LibreHardwareMonitor`; the machine also has an
  install at `C:\Tools\LibreHardwareMonitor`. It must be started with **Start it** or
  the **Start LibreHardwareMonitor with QuantDock** setting.
- OpenCode: three entries in `%USERPROFILE%\.config\opencode\opencode.json` — `local`,
  `local-3d`, `local-twil`. Do not rename the aliases without updating the model TOMLs.

## Architecture notes

- The manager polls `server::status()` (3 s cache, 1 s when the server is down) and
  probes LHM every 5 s on a worker thread. It does not sample resource values itself.
- The overlay runs a sampler thread, sends `Sample` through a channel, and repaints at
  the configured `monitor.interval_ms` (clamped 250–2000 ms).
- `server::props_cached()` is cached 10 s. `status()` caches a serving result 3 s and a
  `None` result 1 s; the cache mutex is never held across the HTTP call.
- The LHM `Reader` reuses one `String` and one `LhmData` across polls. Its JSON nodes use
  `Cow<'a, str>` because LHM escapes `°` as `\u00B0`; a plain `&str` makes serde reject
  the whole document.
- The sampler refreshes LHM every `sensors.interval_ms` (default 2000) but builds a
  sample every `monitor.interval_ms` (default 1000). It caches the last good LHM sample
  and applies it on every tick; a failed refresh keeps it for 10 s, then shows `--`.
- The sensor manager uses a picker-only `monitor_lhm::fetch_tree` for the hardware tree;
  the sampler's `Reader`/`LhmData` are separate. The tree is rebuilt while the window is
  open and opens CPU/GPU Temperatures by default.
- Settings are draft-based: every control edits an in-memory draft; **Save** writes,
  **Return** reloads from disk and discards. A dirty draft ignores external file
  changes and shows a notice.
- The log console tails at most the last 64 KB / 200 lines of the current or last-loaded
  model's `llama-server.log`; it never clears or edits the file.
- The tray is a `tray-icon` icon plus its hidden message window. A wake thread polls menu
  events every 100 ms and checks the second-instance show event, then asks for a repaint
  only when one arrives, so the idle cadence stays at 2 s. X is intercepted only while the
  tray exists; without it X is a real close.

## Invariants and gotchas

- Windows-only. Do not add cross-platform fallbacks or new dependencies for them.
- Both binaries are GUI-subsystem; use `quantdock::hidden()` for child processes so no
  console flashes.
- Display scaling is 150% on this machine. Overlay placement must use physical pixels
  via the HWND; `GetWindowRect` from a DPI-unaware process is logical.
- The overlay is undecorated, always-on-top and `with_taskbar(false)`. It has no
  taskbar entry and no minimize. The manager has a standard Windows title bar.
- OpenCode can paint over always-on-top windows. A "stray square with an X" is usually
  another window showing through the transparent corner, not a QuantDock bug.
- LHM requires UAC. Autostart is one-shot at manager launch; cancelling UAC is not
  retried until the next launch.
- The overlay reads LHM independently; the manager's probe and the overlay's sampler use
  separate processes and separate `Reader`s. The manager starting does not feed the
  overlay.
- Do not reintroduce `refresh_memory()`/`K32GetPerformanceInfo` in the sampler; use
  `sysmem::read()`. Do not read a whole log; use `server::tail`.
- Memory is measured, not assumed. The app's live heap is flat; the residual commit is
  the NVIDIA OpenGL driver (`nvoglv64`/`dxgmms2`) and is waived as a platform floor.
  Do not hide numbers with working-set trimming.

## Current state (2026-11-18)

Shipped on `main`:

- Model reorg and 6 loaders; per-model TOML; 3 OpenCode entries.
- Overlay config: rows, text/bar/graph, thresholds, themes, width, placement.
- Draft Settings with Save/Return and a disk-change notice.
- LHM setup, autostart, custom sensor fields, missing-sensor marker.
- llama-server log console; log tail is bounded.
- Sensor manager window (all built-ins + LHM), custom field config table, ten profiles with Load anyway/Cancel.
- Hardware-grouped sensor tree (CPU/GPU/Memory/Board/Storage/Network/Other).
- Memory churn fixes: `sysmem`, 5 s process refresh, 3 s status cache, shared LHM
  reader, halved repaints.
- LHM escaped-JSON fix and sampler LHM cache fix.
- Centered 640 px Settings column (clamped to the window width, full-height scroll).
- Hugging Face download: repo/URL parse, GGUF listing, streaming download with Cancel
  and `.part` cleanup, generated TOML, startup sweep; `src/hf.rs`.
- Tray icon: Show/Hide, live Start/Close monitor, live tooltip, X hides to the tray,
  Exit state machine (dirty-draft confirm, cancel download ≤ 5 s, close overlay).
- Single instance: named mutex + show event; a second launch reveals the window and exits.
- **Start hidden to tray** setting (`Manager` section).

Measured: 10-minute plateau was 3.22 MB/min (manager) and 3.55 MB/min (overlay), still
monotonic; VIRT_ALLOC attributes the residual to the NVIDIA driver. This is documented
as a waiver, not a passing gate.

## Open work / where to pick up

1. **Memory driver floor.** Waived. If revisiting, use the xperf/WPA workflow in the
   memory plan and the VIRT_ALLOC finding before changing code. The next suspects are
   `refresh_cpu_usage` every tick, `Disks::new_with_refreshed_list()` when `disk_free`
   is visible, and the per-tick `nvidia-smi` spawn.
2. **Review gaps.** No automated hit/miss test for the status cache and no concurrency
   test for the shared LHM `Reader`; both need a loopback/LHM harness. The TTL policy
   and first-tick refresh have unit tests.
3. **Sensor-tree polish.** Whitespace-only search force-opens every branch; a nested
   hardware header salts on the remaining path suffix, so duplicate trailing segments
   could collide; the tree is rebuilt per repaint while the picker is open. None are
   correctness blockers.
4. **Sensor-manager crash.** The old inline picker is gone, but the crash was never
   reproduced. Check `%APPDATA%\QuantDock\crash.log` and the new window before claiming
   it fixed.

## Debug recipes

**All LHM fields show `--`:**
1. `Invoke-WebRequest http://127.0.0.1:8085/data.json` — expect 200 and ~310 sensors.
2. `cargo test --all-targets monitor_lhm` — the escaped-degree test must pass.
3. `cargo test --all-targets sampler_keeps -- --ignored` with LHM running — four
   consecutive samples must all contain `cpu_temp`.
4. If the endpoint works but the parser fails, check the raw JSON for `\u00B0` escapes
   and make sure `Node` still uses `Cow<'a, str>`.

**Memory:** build release, run both binaries, sample `PrivateMemorySize64` every 30 s for
10 minutes. For allocation stacks, use xperf/WPA:
`xperf -on PROC_THREAD+LOADER` plus a `HeapSession` with `-heap -Pids <pids>
-stackwalk HeapAlloc+HeapRealloc+HeapCreate`, then `xperf -merge`. Use `-a heap -stacks
st` for total churn and `-a virtualalloc -stacks soc` for driver/private memory.

**Overlay not visible/updating:** check `Get-Process monitor`, the named mutex
`Local\QuantDockMonitor`, the overlay's `monitor_up` button state, and the config's
`monitor.interval_ms`. The overlay is not in the taskbar by design.

**Manager window will not appear:** X hides it when the tray is up — use **Show QuantDock**
on the tray icon. A second `quantdock.exe` only signals `Local\QuantDockManagerShow`, so a
second copy never opens a second window. If the first launch hid itself, check
`start_hidden_to_tray` in `settings.toml`; the tray-less fallback ("tray icon unavailable")
keeps the window visible on purpose.

**Crash:** both binaries install a panic hook that appends the panic message and a
backtrace to `%APPDATA%\QuantDock\crash.log`. Read it before guessing.

## Documentation rules

- User-facing behavior, manual install steps and the agent install prompt go in
  `README.md`.
- The detailed agent-facing install procedure, model downloads and per-model setup go in
  `INSTALL.md`.
- Design and measurements go in `docs/superpowers/specs/`.
- Execution plans and results go in `docs/superpowers/plans/`.
- Keep `AGENTS.md` as the index; update it when the current spec, commands, or open work
  changes.
