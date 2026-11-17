# Manager polish: centered settings, HuggingFace download, tray icon

**Date:** 2026-11-17
**Status:** Design — approved approach, not yet planned or built

## Intent

Three manager (`src/bin/quantdock.rs`) improvements, all user-facing:

1. **Centered settings page.** The Settings tab currently fills the window edge to
   edge, which reads badly on wide windows. Constrain it to a readable column and
   center it.
2. **Download models from HuggingFace.** Today the only way to add a model is to
   hand-build a folder with a `.gguf` and a matching `.toml` under the model root.
   Add an in-app downloader (Option A: repo + file picker, no HF search) that
   downloads one GGUF and generates the TOML, so a new model is two clicks away.
3. **Tray icon for the manager.** `quantdock.exe` disappears completely when the
   window closes, even while llama-server is serving. Add a system tray icon with
   show/hide, start/stop monitor and exit, and make the X button hide to tray
   instead of exiting.

## Goals

- Settings content is capped to a centered column on every window size.
- Paste a repo id or URL, pick a GGUF, watch progress, cancel if needed, and end
  up with a discoverable model card (folder + `.gguf` + `.toml`) in the model root.
- Manager minimizes/closes to the tray; the tray menu starts and stops the
  monitor overlay using the exact code paths the Monitor button already uses.
- Only one manager can run at once: a second launch of `quantdock.exe` shows the
  existing window instead of starting a second manager (Ruling: guard —
  hide-to-tray makes double-launch likely; two managers would race port 8080 and
  `settings.toml`).
- Tray menu clicks act instantly even while the window is hidden (Ruling: wake
  thread — a tiny thread unblocks the repaint when a menu event arrives).
- **Exit** with a download running cancels the download and cleans the partial
  file first (Ruling: cancel-and-clean — a leftover `.part` would force the next
  attempt into a `-2` folder).
- A settings checkbox starts the app hidden in the tray (user-picked neighbour).
- The tray tooltip shows the current model/server and monitor state
  (user-picked neighbour).

## Non-goals

- No HuggingFace search, ranking or browsing — the user already knows the repo.
- No resume of a partial download; cancel deletes the partial file.
- No authentication (gated repos are out) and no LFS pointer chasing beyond what
  the plain `resolve` URL gives.
- No split-GGUF support (`-00001-of-00003` shards); one file per download.
- No download queue — one download at a time (declined neighbour: half a day for
  multi-repo queueing; nothing asked for it).
- No cross-platform code paths beyond what exists today.
- No auto-update, no installer, no overlay changes.

## Anti-goals (would count as failure)

- Blocking the UI or repainting with a multi-GB download on the main thread.
- Closing the window killing a running llama-server without warning (that is
  exactly the behavior the tray exists to fix).
- Writing a model folder the discovery pass would not find (`<folder>\<folder>.toml`
  must always exist; a download that produces an invisible model is a bug).

## Constraints

- Windows-only, Rust 2024, egui/eframe 0.37, GUI-subsystem binaries; child
  processes via `quantdock::hidden()`.
- `ureq` 3.x with the `json` feature is already a dependency — the HF client uses
  it; no HTTP dependency is added.
- Exactly one new dependency for the tray: `tray-icon` (current release line is
  0.26.x; builder picks the exact version). Its menu events are polled in
  `update()` per frame; the one extra thread allowed is the wake-up thread below,
  which only calls `ctx.request_repaint()` and never touches the window or app
  state.
- Model root is `C:\aiModels` or `QUANTDOCK_MODELS`; same constant the discovery
  pass uses (`src/model.rs`).
- Config paths in `monitor_config` are unchanged; this feature stores no new
  persisted state. A cancelled or failed download must leave no model folder and
  no partial `.toml` behind (the `.part` file is deleted on failure/cancel).

## Approaches

### Centered settings

- **A. Max-width + center inside the existing CentralPanel (chosen).** Cap the
  content column (~640 px), offset it by half the leftover width. Cheap, keeps the
  tab strip and Save/Return where they are.
- **B. Popup window for settings.** Rejected: the draft Save/Return model, the
  disk-change notice and the picker window all assume the current panel; a window
  adds lifetime management for no gain.
- **C. Two-column layout at large widths.** Rejected: reorders controls and
  touches every section for a purely cosmetic ask.

### HuggingFace download

- **A. Repo id or URL + file list + single download (chosen, user-approved).** One
  text input; `GET /api/models/{repo}/tree/main` lists files; the user picks one
  GGUF; a background thread streams it and the app writes the TOML.
- **B. Full HF browser with search.** Rejected as a non-goal; HF's own site is the
  search tool.
- **C. Paste a direct `.gguf` resolve URL only.** Rejected: the file listing is one
  cheap API call and gives correct names and sizes for free.

### Tray

- **A. `tray-icon` with menu events polled in `update()` (chosen).** Fits eframe:
  create the tray once, poll `MenuEvent::receiver().try_recv()` per frame, no extra
  event loop or thread.
- **B. A `tray-icon`-provided event-loop integration or separate thread.**
  Rejected: two event loops on the same thread is unsound; a second thread
  touching the window handle races eframe's repaint.
- **C. Win32 `Shell_NotifyIcon` by hand through the `windows` crate.** Rejected:
  message-window plumbing, icon and menu code for what `tray-icon` gives in ~50
  lines.

## Design

### 1. Centered settings

In the Settings branch of the CentralPanel, before calling `settings_ui`:

- Cap the working width at ~640 px (builder may tune; the reference point is that
  the widest settings row fits without wrapping).
- Offset from the left by `(available_width - cap) / 2` so the column is centered,
  and keep `ui.set_max_width` so controls stop at the cap.
- The invalid-`settings.toml` banner and the Save/Return row stay at the top of
  that column; nothing moves between sections.

### 2. HuggingFace downloader

New module `src/hf.rs` (lib), used only by the manager binary.

**Input parsing** (`hf::parse_repo(input) -> Option<String>`). Accepts:

- `owner/name`
- `https://huggingface.co/owner/name`
- the same URL with `/tree/main`, `/tree/<branch>`, `/blob/...`, or a trailing
  query (`?filename=...`) — the filename part is dropped; the picker re-selects.

Output is the normalized `owner/name`. Unit-tested.

**File listing** (`hf::list_ggufs(&repo) -> Result<Vec<HfFile>, String>`).

- `GET https://huggingface.co/api/models/{repo}/tree/main?recursive=true` via
  `ureq`; entries with `type == "file"` and a `.gguf` (case-insensitive) suffix are
  kept, with `path` and `size`.
- A repo with no GGUFs is an error message, not an empty list.
- Runs on a worker thread; the dialog shows a spinner while it runs.

**Download flow** (`hf::Downloader` or equivalent state owned by `Manager`).

- UI: a "Download" button in the Models tab opens a small dialog (same pattern as
  the profile dialog): input + "Fetch files" → radio list `name  (size)` →
  "Download" → progress bar (`downloaded / total`, MB and %) + Cancel.
- One download at a time; the dialog is disabled while one is running.
- Background thread: `GET https://huggingface.co/{repo}/resolve/main/{path}` and
  stream body chunks to `<root>\<folder>\<gguf>.part`, updating shared progress
  (bytes, total, done flag, error string). `folder` = repo name part of the path
  (`bartowski/Qwen3-8B-GGUF` → `Qwen3-8B-GGUF`); if a sibling file already exists
  in it, a numeric suffix is appended (`…-2`) instead of merging into the existing
  model. On completion, rename `.part` → final name.
- Cancel: a shared flag the thread checks per chunk; the `.part` file is deleted
  and the folder removed if it holds nothing else.
- **Exit mid-download:** tray Exit sets the cancel flag and waits (bounded, a few
  seconds) for the thread to acknowledge and clean up before quitting. If the
  thread ignores it in that window, exit proceeds anyway and the leftover is
  documented in README.
- The download sets an explicit read timeout on the request so a stalled server
  cannot wedge the thread past the per-chunk cancel check.
- On success: write `<folder>\<folder>.toml` with `name` = gguf stem, `gguf` =
  filename, `alias` = folder name, `args` = empty. The user edits alias/args on the
  model card as they do today. Then `reload()` the model list and show a note.
- Errors (HTTP, disk full, repo not found) surface as the existing red note and
  keep the dialog open with the list still populated.

**State shape is the builder's call**, but progress must travel through a channel
or shared `Arc<Mutex<…>>` read by the repaint — never a `Mutex` held across a
network call, matching the codebase's existing cache rules.

### 3. Tray icon and hide-to-tray

**New dependency:** `tray-icon` (latest 0.19.x-compatible release; builder picks
the exact version). No icon asset file — draw the tray icon at runtime into a
small RGBA buffer (simple rounded square with a "Q" or the existing app aesthetic;
builder's call).

**Behavior:**

- Tray created at startup with tooltip "QuantDock".
- Menu:
  - **Show / Hide QuantDock** (label reflects current visibility)
  - **Start monitor** / **Close monitor** (exactly one visible; built from the
    current `monitor_up` state each time the menu opens, or via
    `set_enabled`/rebuild — builder's call, it must never show a start action
    while the overlay is up)
  - separator
  - **Exit**
- Tooltip: shows the running model/server state and whether the monitor is up,
  e.g. `QuantDock — serving Qwen3-8B · monitor up`. Updated per repaint from the
  state the manager already holds; a cheap `set_tooltip` only when the text
  changes.
- Wake thread: one thread started at tray creation, blocked on a second receiver
  for menu events, calling `ctx.request_repaint()` per event so a hidden window
  sees clicks instantly. It touches nothing else.
- Window close (X): hides the window to the tray instead of exiting. The window
  keeps running, llama-server keeps serving, the settings draft is kept.
- Minimize is unchanged (normal Windows minimize). Double-click on the tray icon
  toggles show/hide (builder may make it always-show; must be consistent).
- **Start hidden to tray:** a persisted setting (new field in the manager
  settings section, draft-edited like every other control). When set, the next
  app launch skips showing the window: it starts hidden with the tray icon
  present; Show from the tray reveals it.
- Menu events: `update()` polls `MenuEvent::receiver().try_recv()` every frame and
  acts on the id; the wake thread only triggers the repaint.
- Show/hide uses the HWND via the existing `raw-window-handle` dependency
  (`ShowWindow`/`SW_HIDE`/`SW_SHOW`/`SW_RESTORE` through `windows`, or
  `set_visible` if eframe 0.37's viewport API supports it from inside the app —
  builder verifies which path compiles cleanly and documents the one used).
- Start/stop monitor call the existing `Self::spawn_monitor()` /
  `Self::close_monitor()`; `monitor_up` polling already exists and the tray menu
  state rides on it.
- Exit: if a download runs, cancel-and-clean (above); destroys the tray, then
  closes the app normally. A running llama-server keeps running (current behavior
  on exit — unchanged).

### 4. Single-instance guard

- Named mutex `Local\QuantDockManager`, created at startup (same pattern as the
  overlay's `Local\QuantDockMonitor`).
- Second launch: cannot create the mutex → it must NOT start its UI loop.
  It signals the first instance to show its window, then exits. The signal is a
  named event (`Local\QuantDockManagerShow`), set and immediately released; the
  first instance waits on it with a zero-timeout check each repaint (no extra
  thread — it rides the existing repaint loop).
- The tray icon therefore exists only in the live instance, and port 8080 and
  `settings.toml` never see two managers.

**Not touched:** the Monitor button, taskbar-less overlay, or `window.toml`
placement.

## Tests

- Unit: `parse_repo` (plain, URL, tree/blob/query forms, garbage → None).
- Unit: GGUF filter picks `type=file` + `.gguf` suffix, ignores others, and
  reports a clear error when nothing matches.
- Unit: TOML generation produces exactly the four keys with defaults; a folder
  named with the numeric-suffix rule still satisfies `<folder>\<folder>.toml`.
- Ignored/manual: a real download of a small public GGUF; tray show/hide and
  monitor start/stop (needs a desktop session — add to the manual checklist in the
  plan, not CI); the single-instance guard's second-launch path (needs a live
  first instance).
- `cargo test --all-targets` stays green; CI unchanged.

## Documentation

- `README.md`: user-facing — Download button behavior, tray behavior (X hides,
  Exit in the tray), and that gated/split GGUF repos are not supported.
- `AGENTS.md`: code map gains `src/hf.rs`; current-state list gains the three
  features; the tray dependency is noted in commands/notes if relevant.
- `Cargo.toml`: + `tray-icon`.

## Left to the builder

- Exact column width and offsets for the centered settings.
- Tray icon pixels and menu item wording.
- Progress-state plumbing details (channel vs shared struct) within the rules
  above.
- eframe 0.37 visibility API choice (documented in the plan).
- Dialog layout within the existing dialog pattern.
