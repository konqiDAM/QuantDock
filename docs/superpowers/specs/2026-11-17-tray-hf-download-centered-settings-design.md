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
  attempt into a `-2` folder). The bounded wait can still strand a `.part` if the
  thread ignores the flag; a startup sweep (below) keeps that from being
  permanent.
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

## Reviewers

`Reviewers: Beam Free (max), Hy4 flash Free (high), DeepSeek V4.1 Flash`

## Review log

Reviewers (roster order): Beam Free (max), Hy4 flash Free (high), DeepSeek V4.1 Flash. Failure policy: stop. Log written once at green.

- **Beam Free** — rounds 1–5. R1: BLOCKER, 9 findings (2 blockers: tray-icon single global receiver event-stealing; hidden-window processing unspecified — fixed via wake-thread→app-owned channel and `App::logic()`; constraint/persist conflict; field home; tray-icon version 0.19 vs 0.26; show-event semantics; folder-suffix rule; double-click; connect timeout). R2: MAJOR (constraint wording; `App::logic` exists — controller's ruling was wrong, corrected after verifying eframe 0.37 `epi.rs`; show latency → ~100 ms checks). R3: MAJOR (move `poll()` to `logic()`; stale wake-thread wording). R4: MAJOR (wake-thread bullet reversed channel roles; exposed auto-reset consumption race → manual-reset consumed only by `logic()`). R5: **CLEAN**.
- **Hy4 flash Free (high)** — rounds 1–2. R1: MAJOR, 5 findings (left-click needs `TrayIconEvent`, human ruled menu-only; stale monitor menu label → `set_text` + fresh re-check; GGUF magic validation; alias default → human ruled `local`; repo named `old` → suffix rule). R2: **CLEAN**.
- **DeepSeek V4.1 Flash** — rounds 1–6. R1: MAJOR, 8 findings (X must intercept via `CancelClose`+`Visible(false)`+`quitting`; Exit/overlay → human ruled Exit closes monitor; `recv_timeout(100ms)`; tray created on winit thread; branch carried through; startup sweep; dirty-draft Exit → human ruled confirm first; start-hidden mechanism). R2: MAJOR (`with_visible` overridden by eframe's first-frame show — verified `epi_integration.rs` → `logic()`-driven hide; Exit confirm unreachable hidden → show first; exit state machine, no blocking; signatures carry branch). R3: MINOR×3 (sweep restricted to `*.gguf.part` + README note; `ViewportCommand::Visible` only, HWND path deleted; Quit sends `Close`). R4: MAJOR+MINOR (`ViewportInfo::visible()` unusable on Windows — verified `viewport_info.rs` → app-owned `hidden: bool`; `ui()`-while-hidden claim reworded). R5: MINOR×2 (Exit confirm reuses the Show action; every reveal path clears `hidden`; Q1 ruled: Hide/Show symmetric). R6: **CLEAN**.

Human rulings recorded in the findings above: menu-only left click, alias `local`, Exit closes monitor, confirm-first on dirty draft, Show restores normal, Hide/Show symmetric.

## Constraints

- Windows-only, Rust 2024, egui/eframe 0.37, GUI-subsystem binaries; child
  processes via `quantdock::hidden()`.
- `ureq` 3.x with the `json` feature is already a dependency — the HF client uses
  it; no HTTP dependency is added.
- Exactly one new dependency for the tray: `tray-icon` (current release line is
  0.26.x; builder picks the exact version). Menu events reach the app only via
  the wake thread into an app-owned channel — the global receiver stays the wake
  thread's alone. The one extra thread allowed is that wake-up thread, which also
  polls the single-instance show event and only ever forwards events or calls
  `ctx.request_repaint()`; it never touches the window or app state.
- Model root is `C:\aiModels` or `QUANTDOCK_MODELS`; same constant the discovery
  pass uses (`src/model.rs`).
- Config paths in `monitor_config` are unchanged **except one new field** (below):
  `start_hidden_to_tray`, top-level on `Config`, default `false`, `#[serde(default)]`
  covers old readers. No other persisted state is added. A cancelled or failed
  download must leave no model folder and no partial `.toml` behind (the `.part`
  file is deleted on failure/cancel).

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
  text input; `GET /api/models/{repo}/tree/{branch}` lists files; the user picks
  one GGUF; a background thread streams it and the app writes the TOML.
- **B. Full HF browser with search.** Rejected as a non-goal; HF's own site is the
  search tool.
- **C. Paste a direct `.gguf` resolve URL only.** Rejected: the file listing is one
  cheap API call and gives correct names and sizes for free.

### Tray

- **A. `tray-icon` with menu events drained in `App::logic()` (chosen).** Fits eframe:
  create the tray once; the wake thread forwards into an app-owned channel;
  `logic()` drains it each pass and runs while the window is hidden (eframe calls
  `logic()` on `request_repaint` even hidden, `epi.rs` App docs).
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

**Input parsing** (`hf::parse_repo(input) -> Option<(String, String)>` — returns
the normalized `owner/name` and the parsed branch). Accepts:

- `owner/name`
- `https://huggingface.co/owner/name`
- the same URL with `/tree/main`, `/tree/<branch>`, `/blob/...`, or a trailing
  query (`?filename=...`) — the filename part is dropped; the picker re-selects.

Output is the normalized `owner/name` **plus the parsed branch** (default
`main`); both the tree listing and the `resolve` download URL use that branch, so
a URL from a non-`main`-default repo works instead of 404ing. Unit-tested.

**File listing** (`hf::list_ggufs(&repo, &branch) -> Result<Vec<HfFile>, String>`).

- `GET https://huggingface.co/api/models/{repo}/tree/{branch}?recursive=true` via
  `ureq`; entries with `type == "file"` and a `.gguf` (case-insensitive) suffix are
  kept, with `path` and `size`.
- A repo with no GGUFs is an error message, not an empty list.
- Runs on a worker thread; the dialog shows a spinner while it runs.

**Download flow** (`hf::Downloader` or equivalent state owned by `Manager`).

- UI: a "Download" button in the Models tab opens a small dialog (same pattern as
  the profile dialog): input + "Fetch files" → radio list `name  (size)` →
  "Download" → progress bar (`downloaded / total`, MB and %) + Cancel.
- One download at a time; the dialog is disabled while one is running.
- Background thread: `GET https://huggingface.co/{repo}/resolve/{branch}/{path}` and
  stream body chunks to `<root>\<folder>\<gguf>.part`, updating shared progress
  (bytes, total, done flag, error string). `folder` = repo name part of the path
- `folder` = repo name part of the path
  (`bartowski/Qwen3-8B-GGUF` → `Qwen3-8B-GGUF`). If that folder exists and
  contains any file — or the derived name is `old` (case-insensitive; discovery
  skips such folders forever) — use `…-2`, `…-3`, … (first free suffix) instead of
  merging into the existing model; an existing but empty folder is reused. On
  completion, verify the first bytes of the `.part` file are the GGUF magic
  (`GGUF`); a gated or moved file can answer HTTP 200 with an HTML login page —
  on mismatch, delete the `.part`, clean the folder, and show the red note.
  Then rename `.part` → final name.
- Cancel: a shared flag the thread checks per chunk; the `.part` file is deleted
  and the folder removed if it holds nothing else.
- **Exit mid-download:** the Exit state machine (above) sets the cancel flag and
  polls the thread from later `logic()` passes with a ~5 s deadline — no blocking
  wait. The thread's ack includes cleanup (`.part` deleted, folder removed if
  empty). If the thread ignores the flag in that window, exit proceeds anyway and
  the startup sweep recovers the leftover later.
- The download sets an explicit connect timeout and global read timeout on the
  request so neither a stalled connection nor a stalled server can wedge the
  thread past the per-chunk cancel check.
- On success: write `<folder>\<folder>.toml` with `name` = gguf stem, `gguf` =
  filename, `alias` = `local` (the default OpenCode entry, so a fresh download is
  reachable from OpenCode immediately; editable on the model card), `args` =
  empty. The user edits alias/args on the model card as they do today. Then
  `reload()` the model list and show a note.
- **Startup sweep:** at startup the manager scans the model root for folders that
  contain only `*.gguf.part` files (our downloader's naming; no `.gguf`, no
  `.toml`, no other files) — stranded partials — and deletes them before
  discovery. Any folder holding a real file is never touched. README notes that
  a manually downloaded `*.gguf.part` left in the model root may be swept
  (alternative: skip the sweep and every retry burns disk with a stale folder).
  This makes the bounded-wait escape recoverable instead of a permanent
  `-2` suffix.
- Errors (HTTP, disk full, repo not found) surface as the existing red note and
  keep the dialog open with the list still populated.

**State shape is the builder's call**, but progress must travel through a channel
or shared `Arc<Mutex<…>>` read by the repaint — never a `Mutex` held across a
network call, matching the codebase's existing cache rules.

### 3. Tray icon and hide-to-tray

**New dependency:** `tray-icon` 0.26.x (builder picks the exact version). No icon
asset file — draw the tray icon at runtime into a small RGBA buffer (simple
rounded square with a "Q" or the existing app aesthetic; builder's call).

**Behavior:**

- Tray created at startup with tooltip "QuantDock".
- Menu:
  - **Show / Hide QuantDock** (label reflects the app-owned `hidden: bool`)
  - **Start monitor** / **Close monitor** (exactly one visible). The label is
    rewritten (`set_text`) in `logic()` whenever `monitor_up` changes; and before
    acting on a monitor menu click, the handler re-checks `Self::monitor_running()`
    fresh (a cheap `OpenMutexW`) rather than trusting the cached `monitor_up` — a
    click on a stale label can never double-spawn the overlay)
  - separator
  - **Exit**
- Tooltip: shows the running model/server state and whether the monitor is up,
  e.g. `QuantDock — serving Qwen3-8B · monitor up`. Updated per repaint from the
  state the manager already holds; a cheap `set_tooltip` only when the text
  changes.
- Wake thread: one thread started at tray creation. Its loop is
  `menu_rx.recv_timeout(100 ms)`: a received event is forwarded into the
  app-owned channel, every timeout (and every event) is followed by a
  zero-timeout check of the single-instance show event (manual-reset — checking
  does not consume it); when either fires it calls `ctx.request_repaint()`. It
  touches nothing else (no window, no app state — forwarding into a
  `std::sync::mpsc` sender is its only side effect).
- Tray creation: the `TrayIcon` is created in `Manager::new()` on the
  eframe/winit thread — tray-icon's Windows message window must live on a
  message-pumping thread, and the winit event loop pumps there. The wake thread
  receives only; it never creates UI objects.
- **All per-frame app maintenance moves to `logic()`.** The existing `poll()`
  (server status, `monitor_up` detection, `settings.toml` watcher, load/download
  results — currently called from the top of `ui()`) moves into `logic()` so it
  keeps running while the window is hidden; tray tooltip text, tray monitor menu
  state, notes and load completions then stay fresh with the window hidden.
  `logic()` runs before every `ui()` call, so no polling is duplicated.
- Window close (X): hides the window to the tray instead of exiting. Mechanism:
  on `close_requested` the app sets `hidden = true`, sends
  `egui::ViewportCommand::CancelClose` (eframe exits the viewport without it,
  killing the tray with the window) and then `ViewportCommand::Visible(false)`.
  The real exit path is a `quitting: bool` the app sets itself — only tray Exit
  (and its confirm dialogs) may exit; X never does. The window keeps running,
  llama-server keeps serving, the settings draft is kept.
- Minimize is unchanged (normal Windows minimize). A left click on the tray icon
  opens the tray menu (standard Windows behavior; Show/Hide lives in the menu).
  tray-icon's icon-click events are not used at all — one event channel
  (`MenuEvent::receiver()`) only.
- **Start hidden to tray:** a persisted setting — top-level
  `start_hidden_to_tray: bool` (default `false`) on `Config` in
  `monitor_config.rs`, draft-edited in the settings UI like every other control
  (NOT part of `Profile` snapshots — it is a manager preference, not overlay
  layout). Mechanism: `main()` pre-reads this one flag from `settings.toml`
  (tolerating a missing or malformed file as `false`; it never writes). eframe's
  `with_visible(false)` is NOT used — eframe unconditionally shows the root
  window after its first painted frame (`epi_integration.rs` `post_rendering`),
  overriding it. Instead the first `logic()` passes send
  `ViewportCommand::Visible(false)` until the app-owned `hidden` flag takes over
  (a small `hide_pending` flag bounds the loop); one frame of flash is accepted. From then
  on the tray icon is the only sign of the app; Show from the tray reveals it.
- Menu events: tray-icon exposes one global `MenuEvent::receiver()`; to avoid the
  wake thread and the app stealing from each other, the wake thread forwards each
  event into an app-owned `mpsc` channel and the app's `App::logic()` override
  (a new method next to the existing `fn ui`) drains only that channel. Event
  handling — show/hide, monitor start/stop, exit — lives in `logic()`; `ui()`
  only renders.
- **Hidden-window behavior:** the app must not depend on `ui()` for any tray
  action — whether `ui()` runs while the window is hidden is an eframe-internal
  detail that differs by integration, so all tray/event processing lives in
  `logic()`, which eframe 0.37 runs while hidden whenever `request_repaint` was
  called (no egui pass, no painting). E.g. the Exit confirm dialog is made
  reachable by explicitly showing the window first, never by assuming `ui()` ran
  hidden. The plan still adds one manual verification (hide, click Start monitor
  in the tray, overlay must appear within ~1 s).
- **Visibility source of truth:** an app-owned `hidden: bool` — set when the
  window hides (X pressed, start-hidden bootstrap), cleared by **every** reveal
  path: tray Show, the single-instance show event, and the Exit confirm's reveal.
  It drives the Show/Hide label, the toggle action, the hide bootstrap and every
  hidden-state check. Tray Hide from a minimized window hides it (removes it from
  the taskbar until Show) rather than being a no-op (Ruling: Hide and Show are
  symmetric — Hide always hides, Show always restores normal). `ViewportInfo::visible()` is NOT usable for this on
  Windows: it derives only from minimized/occluded (`egui`
  `viewport_info.rs`), and winit emits `Occluded` on other platforms only — a
  hidden window reports `None` forever, which would strand the user in the tray.
- Tray Show: sends `Visible(true)` + `Minimized(false)` + `Focus` — Show always
  restores to a normal window, even if the user had minimized it (Ruling: Show
  means "show it", not "undo only the hide").
- Start/stop monitor call the existing `Self::spawn_monitor()` /
  `Self::close_monitor()`; `monitor_up` polling already exists and the tray menu
  state rides on it.
- **Exit** is a small state machine driven by `logic()` — it never blocks the
  egui thread (a blocking wait would freeze window and tray and trip Windows'
  not-responding). States, in order:
  1. **Confirm:** if the Settings draft is dirty, first run the same Show action
     as tray Show (`Visible(true)` + `Minimized(false)` + `Focus`, clearing
     `hidden`) — a minimized window would otherwise never paint the dialog — and
     show it: "Exit without saving settings?" Proceed on confirm; cancel returns
     to idle. (Ruling from the earlier "confirm first" decision: popping the
     window up to ask is what that choice means when hidden.)
  2. **Cancel download:** if a download runs, set the cancel flag and record a
     deadline (~5 s); later `logic()` passes poll for the thread's acknowledgement
     and clean up the `.part`; when done or the deadline passes, continue.
  3. **Close monitor:** if the monitor is up, call the existing
     `Self::close_monitor()` so the taskbar-less overlay is never orphaned
     (human-ruled: Exit is a full stop).
  4. **Quit:** set `quitting = true`, destroy the tray, then send
  `ViewportCommand::Close` — with `quitting` set the close handler stops
  cancelling and the viewport actually closes. A running
  llama-server keeps running (current behavior on exit — unchanged).

### 4. Single-instance guard

- Named mutex `Local\QuantDockManager`, created at startup (same pattern as the
  overlay's `Local\QuantDockMonitor`).
- Second launch: cannot create the mutex → it must NOT start its UI loop.
  It signals the first instance to show its window, then exits. The signal is a
  named **manual-reset** event (`Local\QuantDockManagerShow`); only `logic()`
  consumes it — it zero-timeout checks the event every pass and calls `ResetEvent`
  when it acts (runs the Show action, which clears `hidden`). The wake thread
  checks it too but never
  consumes it; it only repaints. A check alone can never steal the show action
  from `logic()`. The wake thread's ~100 ms checks mean a second launch shows the
  existing window within ~100 ms even while hidden.
- The tray icon therefore exists only in the live instance, and port 8080 and
  `settings.toml` never see two managers.

**Not touched:** the Monitor button, taskbar-less overlay, or `window.toml`
placement.

## Tests

- Unit: `parse_repo` (plain, URL, tree/blob/query forms, garbage → None).
- Unit: GGUF filter picks `type=file` + `.gguf` suffix, ignores others, and
  reports a clear error when nothing matches.
- Unit: TOML generation produces exactly the four keys with defaults, with
  `alias` = `local`; a folder named with the numeric-suffix rule still satisfies
  `<folder>\<folder>.toml`; a derived folder named `old` gets a suffix instead.
- Unit: the GGUF magic check rejects non-GGUF bytes before the rename.
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
- Tray icon pixels and menu item wording.- Progress-state plumbing details (channel vs shared struct) within the rules
  above.
- Dialog layout within the existing dialog pattern.
