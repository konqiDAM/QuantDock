# Manager Polish: Centered Settings, HF Download, Tray — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Center the Settings page, add a HuggingFace GGUF downloader, and add a tray icon with hide-to-tray, start/stop monitor and a single-instance guard.

**Architecture:** One new lib module `src/hf.rs` owns all HuggingFace logic (URL parsing, tree listing, streaming download, TOML generation, partial-file sweep) so the UI binary only wires state. Tray, wake thread, single-instance guard and the `logic()`/`ui()` split live in `src/bin/quantdock.rs` because they are tied to eframe's `App` impl. One new field on `Config` (`start_hidden_to_tray`).

**Tech Stack:** Rust 2024, eframe/egui 0.37, ureq 3.x (already present), `tray-icon` 0.26.x (new), `windows` crate (already present).

**Spec:** `docs/superpowers/specs/2026-11-17-tray-hf-download-centered-settings-design.md` — the authority; the plan argues from it.

**Builder model:** subagents implement tasks with `opencode-go/deepseek-v4.1-flash#high` (human-selected).

## Global Constraints

- Windows-only; no cross-platform fallbacks; GUI-subsystem binaries; child processes via `quantdock::hidden()`.
- Exactly one new dependency: `tray-icon` 0.26.x (builder picks exact version). No HTTP dependency (ureq 3.x with `json` feature is already in Cargo.toml).
- `src/model.rs` root rule: models live at `model::models_root()` (`C:\aiModels` or `QUANTDOCK_MODELS`); discovery only finds `<folder>\<folder>.toml`; folder `old` is skipped forever.
- New persisted state is exactly one field: `start_hidden_to_tray: bool`, top-level on `Config`, default `false` (serde default covers old readers). Not part of `Profile` snapshots.
- Downloaded model TOML keys: `name` = gguf stem, `gguf` = filename, `alias` = `local`, `args` = empty.
- Download URLs use the parsed branch: tree `GET https://huggingface.co/api/models/{repo}/tree/{branch}?recursive=true`, file `GET https://huggingface.co/{repo}/resolve/{branch}/{path}`.
- The `.part` file is `<gguf filename>.part` (ends in `.gguf.part`); on failure/cancel it is deleted; the folder is removed if it holds nothing else.
- Tray events: one channel only — `MenuEvent::receiver()` drained solely by the wake thread, forwarded into an app-owned `mpsc`; `TrayIconEvent` unused (left click opens the menu).
- Visibility: `egui::ViewportCommand::Visible` for every hide/show; never raw `ShowWindow`/HWND calls. App-owned `hidden: bool` is the source of truth; cleared by every reveal path.
- Tray/menu/event processing runs in `eframe::App::logic()`; `ui()` only renders. `poll()` moves to `logic()`.
- Close: X → `hidden = true`, `ViewportCommand::CancelClose`, then `ViewportCommand::Visible(false)`. Exit = state machine (confirm dirty draft via the Show action → cancel download with ~5 s deadline → `close_monitor()` → `quitting = true`, destroy tray, `ViewportCommand::Close`).
- LHM and llama-server behavior are untouched; llama-server keeps running on exit.

## Review Focus

Five inputs the spec implies but no automated test fully exercises — each pinned below to the task that owns it.

1. **Gated/renamed HF repo answers HTTP 200 with HTML** — expect a red note, no `.gguf`/`.toml` written. Test: GGUF-magic check unit test (Task 2).
2. **Second launch of quantdock.exe while hidden** — expect the existing window to appear within ~100 ms, not a second manager. Manual checklist item (Task 5) + named-mutex pattern matches the overlay's `Local\QuantDockMonitor`.
3. **Repo whose default branch is not `main`** — expect the download to use the pasted branch, not 404. Test: `parse_repo` branch extraction (Task 1).
4. **Malformed/missing `settings.toml` at launch with start-hidden set** — expect default `false`, window visible, file never overwritten. Test: `Config` default/round-trip (Task 3).
5. **Exit mid-download with the window hidden** — expect the exit state machine to cancel, sweep or close cleanly; never block the UI thread. Manual checklist item (Task 6); the `logic()` no-blocking rule is structural.

---

### Task 1: `src/hf.rs` — URL parsing and file listing

**Files:**
- Create: `src/hf.rs`
- Modify: `src/lib.rs` (add `pub mod hf;`)

**Interfaces:**
- Consumes: `ureq` (already a dependency), `serde_json`.
- Produces:
  - `pub fn parse_repo(input: &str) -> Option<(String, String)>` — returns `(owner/name, branch)`, branch default `main`.
  - `pub struct HfFile { pub path: String, pub size: u64 }`
  - `pub fn list_ggufs(repo: &str, branch: &str) -> Result<Vec<HfFile>, String>` — GETs the tree API, keeps `type == "file"` entries whose path ends `.gguf` (case-insensitive), sorted by path; error message includes the HTTP status; a repo with zero GGUFs returns a distinct error ("no .gguf files in {repo}").

- [ ] **Step 1: Write failing tests in `src/hf.rs` `#[cfg(test)]`**

```rust
#[test] fn parse_repo_plain_id()            // "owner/name" -> ("owner/name", "main")
#[test] fn parse_repo_url_tree_main()       // "https://huggingface.co/o/n/tree/main" -> ("o/n", "main")
#[test] fn parse_repo_url_tree_branch()     // ".../tree/dev?filename=x.gguf" -> ("o/n", "dev")
#[test] fn parse_repo_url_blob()            // ".../blob/main/a.gguf" -> ("o/n", "main")
#[test] fn parse_repo_garbage_is_none()     // "", "a", "https://example.com/x" -> None
#[test] fn tree_json_keeps_only_gguf_files() // feed the tree-API JSON shape below through the filter:
// entries: {"type":"file","path":"a.gguf","size":10}, {"type":"file","path":"readme.md","size":1},
// {"type":"directory","path":"sub"}, {"type":"file","path":"B.GGUF","size":20}
// -> [a.gguf (10), B.GGUF (20)] sorted by path
#[test] fn tree_json_no_ggufs_is_error()    // list-shaped JSON with no .gguf -> Err containing "no .gguf"
```

Keep the filter as a separate pure function (e.g. `fn ggufs_from_tree(json: &serde_json::Value) -> Result<Vec<HfFile>, String>`) so the network call is thin and testable.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --all-targets hf`
Expected: FAIL — `hf` module does not exist.

- [ ] **Step 3: Implement `parse_repo` and `ggufs_from_tree` + `list_ggufs`**

`parse_repo`: accept `owner/name`, `https://huggingface.co/owner/name`, and the same with `/tree/<branch>`, `/blob/...`, `/resolve/...`, optional `?query`. Strip scheme/host, drop query, take first two path segments as repo, take `/tree/` or `/blob/` segment after them as branch (default `main`). Garbage → `None`.

`list_ggufs`: `ureq::get(url).call()` → body `serde_json::from_str::<serde_json::Value>` → `ggufs_from_tree`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets hf`
Expected: PASS (all seven).

- [ ] **Step 5: Commit**

```bash
git add src/hf.rs src/lib.rs
git commit -m "hf: repo/URL parsing and GGUF tree listing"
```

---

### Task 2: `src/hf.rs` — download machinery, TOML generation, startup sweep

**Files:**
- Modify: `src/hf.rs`
- Test: `src/hf.rs` `#[cfg(test)]` (same file)

**Interfaces:**
- Consumes: Task 1's `parse_repo`/`HfFile`; `model::models_root()`; `toml_edit` (already a dependency).
- Produces:
  - `pub struct DownloadSpec { pub repo: String, pub branch: String, pub path: String, pub size: u64 }`
  - `pub fn folder_name(spec: &DownloadSpec, root: &std::path::Path) -> String` — repo-name folder; first free `…-2`/`…-3` suffix when the folder exists and contains any file, or when the name is `old` (case-insensitive); an existing empty folder is reused.
  - `pub enum Progress { Listing, Downloading { bytes: u64, total: u64 }, Done { folder: PathBuf, toml: PathBuf }, Failed(String), Cancelled }`
  - `pub fn start_download(spec: DownloadSpec, root: &std::path::Path, cancel: Arc<AtomicBool>, tx: mpsc::Sender<Progress>)` — spawns the background thread. Streams to `<root>\<folder>\<name>.part`; verifies the first 4 bytes are `GGUF` before renaming; on mismatch/HTTP/disk error → `Failed(msg)` with cleanup; cancel flag checked per chunk → `Cancelled` with cleanup; success → rename, write `<folder>\<folder>.toml` via `toml_edit`, `Done`.
  - `pub fn sweep_partials(root: &std::path::Path) -> Vec<PathBuf>` — deletes folders containing only `*.gguf.part` files (no `.gguf`, `.toml`, or other files); returns the folders it deleted (for a note). Never touches a folder with any real file.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn folder_name_appends_suffix_when_folder_exists_and_has_files()   // tmp with "X/f.gguf" -> "X-2"
#[test] fn folder_name_reuses_empty_folder()                               // empty "X" -> "X"
#[test] fn folder_name_suffixes_old()                                      // "old" repo -> "old-2"
#[test] fn write_toml_has_the_four_keys()                                  // written file parses as ModelConfig:
                                                                           // name=stem, gguf=filename, alias="local", args=""
#[test] fn gguf_magic_check_rejects_html()                                 // bytes b"<html>..." -> Failed-style error, .part deleted
#[test] fn cancel_cleans_the_part_file()                                   // temp root, pre-set cancel flag -> thread
                                                                           // fast-fails BEFORE the request (top-of-thread
                                                                           // check), returns Cancelled, no .part, empty
                                                                           // folder removed — no network in the test
#[test] fn sweep_deletes_only_part_only_folders()                          // folder1 with only "a.gguf.part" deleted;
                                                                           // folder2 with "b.gguf.part"+"c.toml" untouched;
                                                                           // folder3 empty untouched
```

For the magic/TOML tests, split the body of the download into pure helpers the thread calls (e.g. `fn finalize_part(part: &Path, final_name: &Path) -> Result<(), String>` doing the magic check + rename, and `fn write_model_toml(folder: &Path, gguf: &str) -> Result<PathBuf, String>`), so no network is needed.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --all-targets hf`
Expected: FAIL — functions do not exist.

- [ ] **Step 3: Implement the helpers, `folder_name`, `start_download`, `sweep_partials`**

`start_download` takes `root: &Path` explicitly (Task 4 passes `model::models_root()`; no implicit root lookup — the downloader must never resolve a different root than discovery). Thread: explicit connect timeout and global read timeout on the ureq request (builder picks values, seconds-scale); per-chunk cancel check; all `Failed`/`Cancelled` paths delete the `.part` and remove the folder if it holds nothing else. TOML written with `toml_edit` so comments/layout stay intact if the user hand-edits later.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets hf`
Expected: PASS (Task 1 + Task 2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/hf.rs
git commit -m "hf: streaming download, toml generation, partial sweep"
```

---

### Task 3: `start_hidden_to_tray` config field

**Files:**
- Modify: `src/monitor_config.rs` (Config struct ~line 316, defaults, tests)
- Modify: `src/bin/quantdock.rs` (settings UI: one checkbox row beside the existing draft controls)

**Interfaces:**
- Consumes: existing `Config` draft Save/Return machinery.
- Produces: `Config.start_hidden_to_tray: bool` (default `false`), serialized top-level in `settings.toml`; a checkbox row in the settings UI that dirties the draft like every other control. NOT added to `Profile`.

- [ ] **Step 1: Write failing tests in `src/monitor_config.rs`**

```rust
#[test] fn start_hidden_defaults_false()          // Config::defaults().start_hidden_to_tray == false
#[test] fn start_hidden_round_trips_through_toml() // set true -> save text -> load -> true (and unknown-field-free)
#[test] fn old_settings_without_the_field_load()  // TOML without the key -> loads, field false (serde default)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --all-targets start_hidden`
Expected: FAIL — field does not exist.

- [ ] **Step 3: Implement**

Field + `Default` + serialization. The struct's existing container-level `#[serde(default)]` covers old files — no field-level attribute is added (Ruling from plan review R1 F9: container-level serde default fills missing fields; the `old_settings_without_the_field_load` test proves it). Settings UI: checkbox labelled "Start hidden to tray" in the settings page (near other manager-level toggles), editing the draft; no `Profile` changes.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: PASS (95 existing + 3 new).

- [ ] **Step 5: Commit**

```bash
git add src/monitor_config.rs src/bin/quantdock.rs
git commit -m "config: start_hidden_to_tray field and settings row"
```

---

### Task 4: Download dialog in the manager UI

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Consumes: Task 1/2's `hf::{parse_repo, list_ggufs, DownloadSpec, start_download, Progress, sweep_partials}`; the profile-dialog `egui::Window` pattern; `Manager::note()`; `Manager::reload()`.
- Produces: a **Download** button in the Models tab (near the existing top-bar controls) opening a dialog with: repo input + "Fetch files"; radio list `name (size)` via `fmt_size`; "Download" → progress bar (bytes/total) + Cancel; one download at a time; success → note + `reload()`; failure → red note, list kept.

- [ ] **Step 1: Write failing test**

```rust
// quantdock.rs tests module: progress bookkeeping as a pure function
#[test] fn dialog_progress_derives_percent_and_text() // (bytes,total) -> percent + "12.3 MB / 40 GB"
```

Test the helper (e.g. `fn progress_label(bytes: u64, total: u64) -> String`), not the drawing.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --all-targets progress_label`
Expected: FAIL.

- [ ] **Step 3: Implement the dialog**

State on `Manager`: `dl: Option<DownloadState>` (Idle{repo}, Listing, Picking{files, selected}, Running{rx, cancel, total}), matching the profile-dialog window pattern. `poll()`-style drain: the dialog consumes `Progress` messages each frame via the existing repaint loop (receiver `try_recv`, never blocking). Wire the **Cancel** button to the `Arc<AtomicBool>`. Call `sweep_partials(model::models_root())` once in `Manager::new()` and surface deleted folders in a note.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "manager: HF download dialog with progress and cancel"
```

---

### Task 5: Single-instance guard, start-hidden bootstrap, `logic()` split

**Files:**
- Modify: `src/bin/quantdock.rs` (Manager impl, `main()`)
- Modify: `Cargo.toml` (no new deps — `windows` crate features already present)

**Interfaces:**
- Consumes: Task 3's `Config.start_hidden_to_tray`; existing `Manager::poll`, `fn ui` (line 2230).
- Produces:
  - Named mutex `Local\QuantDockManager` (create) and manual-reset event `Local\QuantDockManagerShow` (create), both via `windows::Win32::System::Threading`.
  - `impl eframe::App for Manager` gains `fn logic(&mut self, ctx, frame)`; `fn ui` drops its `self.poll(&ctx)` call. `logic()` runs `poll()`, drains the download `Progress` channel (Task 4's dialog state — draining lives here, never in `ui()`, so downloads progress while hidden), the show-event zero-timeout check (acts via the Show action, `ResetEvent`), and the hide bootstrap (`hide_pending` sends `Visible(false)` until `hidden` is set; one-frame flash accepted). `hidden = start_hidden` seeded in `Manager::new` before the first pass.

- [ ] **Step 1: Write failing test**

```rust
#[test] fn mutex_names_match_the_documented_constants() // guards drift: the two names are
// "Local\QuantDockManager" and "Local\QuantDockManagerShow", asserted via constants the code uses
```

(Testing `main()`'s Win32 flow needs a live desktop; the real check is the manual checklist. The constant test keeps the names pinned.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --all-targets mutex_names`
Expected: FAIL.

- [ ] **Step 3: Implement**

Constants for both names; `main()` creates mutex/event, keeps the handles process-lifetime (leaked `Box` or `OnceLock`), reads the flag, calls `Manager::new(start_hidden)`; `Manager::new` seeds `hidden = start_hidden`; `logic()` override; move `poll()`; drain Task 4's `Progress` channel here; bootstrap. Tray is NOT in this task — the show event's handler is wired to the Show action helper (defined here as `fn show_window(&mut self, ctx)` sending `Visible(true) + Minimized(false) + Focus` and clearing `hidden`); the tray tasks reuse it. The `sweep_partials` call added in Task 4's `Manager::new` must survive this rewrite.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "manager: single-instance guard, start-hidden bootstrap, logic() split"
```

---

### Task 6a: Tray icon and wake thread

**Files:**
- Modify: `src/bin/quantdock.rs`
- Modify: `Cargo.toml` (+ `tray-icon = "0.26"` exact minor per crates.io; builder pins)

**Interfaces:**
- Consumes: Task 5's `show_window`, mutex/event handles, `logic()`.
- Produces:
  - Tray created in `Manager::new()` on the eframe/winit thread (message pump owner): menu = Show/Hide QuantDock · Start monitor/Close monitor · separator · Exit; placeholder labels are fine here. The `TrayIcon` handle is stored in a `Manager` field (e.g. `_tray`) — an unstored handle drops immediately and the icon vanishes; the Quit state (6c) drops it to remove the icon.
  - Wake thread: created on the FIRST `logic()` pass, gated by a `wake_started: bool` field on `Manager` (repeat passes must not spawn duplicates), receiving a cloned `egui::Context` (`Context` is `Send + Sync`; `request_repaint` is thread-safe) — not in `Manager::new`, where no context exists. Loop: `MenuEvent::receiver().recv_timeout(100 ms)` → forwards into the app-owned `mpsc`; on event and on every timeout, zero-timeout checks the show event and calls `ctx.request_repaint()`. Nothing else.

- [ ] **Step 1: Write failing test**

```rust
#[test] fn tray_menu_has_the_four_items() // pure fn listing the item ids/labels; guards the menu shape
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --all-targets tray_menu`
Expected: FAIL.

- [ ] **Step 3: Implement**

Icon drawn at runtime into a small RGBA buffer (simple rounded square with a "Q"). Tray, menu ids, app-owned `mpsc`, wake thread with the 100 ms loop. Menu clicks are only received, not yet acted on beyond Show/Hide (which Task 5's `show_window` already supports).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/quantdock.rs Cargo.toml
git commit -m "manager: tray icon and wake thread"
```

### Task 6b: Monitor menu state and live tooltip

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Consumes: Task 6a's menu ids and channel; existing `spawn_monitor`, `close_monitor`, `monitor_up`.
- Produces:
  - Start/Close monitor labels rewritten (`set_text`) in `logic()` whenever `monitor_up` changes; a monitor menu click re-checks `Self::monitor_running()` fresh before acting (never trusts the cache).
  - Tooltip `QuantDock — serving {alias} · monitor up/down`, rewritten only when the text changes.

- [ ] **Step 1: Write failing test**

```rust
#[test] fn tooltip_text_matches_state() // pure fn tooltip_text(serving: Option<&str>, monitor_up: bool)
                                        // composed from two pinned parts, all four combos covered:
                                        // server part: Some -> "serving {alias}", None -> "server down"
                                        // monitor part: "monitor up" / "monitor down"
#[test] fn exit_deadline_is_bounded()   // pure fn or const: EXIT_DEADLINE_MS == 5000 (used by 6c)
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --all-targets tooltip_text`
Expected: FAIL.

- [ ] **Step 3: Implement** label rewriting, fresh re-check, tooltip rewriting.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "manager: tray monitor menu state and live tooltip"
```

### Task 6c: Close interception and exit state machine

**Files:**
- Modify: `src/bin/quantdock.rs`

**Interfaces:**
- Consumes: Task 6a's channel; Task 4's `dl` state (cancel); Task 5's `show_window`; `Config` draft dirty flag.
- Produces:
  - X handling: `close_requested` → `hidden = true`, `ViewportCommand::CancelClose`, `Visible(false)`.
  - Exit state machine in `logic()`: confirm (Show action + dirty-draft dialog "Exit without saving settings?") → cancel download (~5 s deadline, polled from later passes) → `close_monitor()` → `quitting = true`, drop the stored tray handle (removes the icon), `ViewportCommand::Close`.

- [ ] **Step 1: Write failing test**

```rust
// 6b's EXIT_DEADLINE_MS test already pins the bound; add:
#[test] fn exit_states_are_ordered() // pure enum ExitStage { Confirm, CancelDownload, CloseMonitor, Quit }
                                     // with next(): Confirm -> CancelDownload -> CloseMonitor -> Quit -> None
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --all-targets exit_states`
Expected: FAIL.

- [ ] **Step 3: Implement** close interception and the four-state machine; the confirm dialog uses the existing dialog pattern.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: PASS.

- [ ] **Step 5: Manual checklist (record results in the commit message)**

Launch release build: X hides (window gone, tray present, still serving), tray Show restores, Start monitor from tray spawns the overlay, Close monitor removes it, double-launch shows the existing window within ~100 ms, Exit with dirty draft confirms, Exit mid-download cancels and cleans. Hide → tray Start monitor → overlay appears within ~1 s.

- [ ] **Step 6: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "manager: close interception and exit state machine"
```

---

### Task 7: Centered settings page

**Files:**
- Modify: `src/bin/quantdock.rs` (`settings_ui`, called from the CentralPanel branch ~line 2352)

**Interfaces:**
- Consumes: existing `settings_ui`.
- Produces: settings content capped at 640 px, centered via left offset `(available - 640) / 2`; banner and Save/Return row stay at the top of that column; nothing moves between sections.

- [ ] **Step 1: Write failing test**

```rust
#[test] fn settings_column_geometry() // pure fn settings_left_offset(avail: f32, cap: f32) -> f32
                                      // avail=1000,cap=640 -> 180; avail<=cap -> 0
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --all-targets settings_left_offset`
Expected: FAIL.

- [ ] **Step 3: Implement**

Apply the offset with `ui.add_space(offset)` and `ui.set_max_width(cap)` inside the settings branch. Builder may tune the cap ±64 px if a row wraps badly; keep it documented in the code comment.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/quantdock.rs
git commit -m "manager: centered settings column"
```

---

### Task 8: Docs and ship preparation

**Files:**
- Modify: `README.md` (Download button behavior incl. gated/split-GGUF caveats and the manual `*.gguf.part` sweep note; tray behavior: X hides, Exit closes monitor + confirms dirty draft, start-hidden setting)
- Modify: `AGENTS.md` (code map gains `src/hf.rs`; current-state gains the four features; open-work list unchanged)
- Modify: `INSTALL.md` (only where the manual install steps mention adding models)

**Interfaces:**
- Consumes: everything shipped in Tasks 1–7.

- [ ] **Step 1: Write the docs per the spec's Documentation section** (user-facing behavior in README; index updates in AGENTS.md; INSTALL.md only if models-onboarding text is affected).

- [ ] **Step 2: Run the full suite**

Run: `cargo test --all-targets` and `cargo build --release`
Expected: all tests pass; both exes build.

- [ ] **Step 3: Commit**

```bash
git add README.md AGENTS.md INSTALL.md
git commit -m "docs: centered settings, HF download, tray"
```

---

## Manual verification (run once on the release build before ship)

1. X hides; tray present; llama-server still serving.
2. Tray Show restores a normal (non-minimized) window; Hide hides even from minimized.
3. Tray Start monitor spawns the overlay; Close monitor removes it; while hidden, Start monitor works within ~1 s.
4. Relaunch quantdock.exe while hidden → existing window appears within ~100 ms, no second manager.
5. Download `TheBloke/TinyLlama-1.1B-Chat-v1.0-GGUF` → `tinyllama-1.1b-chat-v1.0.Q2_K.gguf` (smallest public file) → card appears with alias `local`, loads, serves.
6. Cancel mid-download → no `.part`, no folder left.
7. Gated repo → red note, nothing written.
8. Exit with dirty draft → confirm dialog; Exit mid-download → cancels, monitor closed, app exits; llama-server still up.
