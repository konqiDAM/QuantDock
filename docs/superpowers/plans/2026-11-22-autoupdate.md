# QuantDock autoupdate Implementation Plan

**Status: done (2026-11-22).** All 10 tasks implemented, reviewed clean by all three reviewers, and verified locally: `cargo test --all-targets` (146 lib + 54 bin passed, 7 ignored), `cargo test --all-targets script -- --ignored` (3 passed), `cargo build --release`.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add automatic GitHub release checking with user-approved, verified, staged replacement of `quantdock.exe` and `monitor.exe` at exit, with rollback.

**Architecture:** A new shared `src/update.rs` owns release JSON parsing, version comparison, streaming download with SHA-256 verification, staging/sweeping, structured markers and swap-script generation. The manager binary owns scheduling, UI and the exit state machine; the swap itself runs in a generated hidden `.cmd` after the manager exits. Settings and release CI gain the small pieces the flow needs.

**Tech Stack:** Rust 2024, egui/eframe 0.37, `ureq` 3 + `serde_json` (already present), `sha2` (new), Windows hidden child processes via `quantdock::hidden()`, `tray-icon`.

**Spec:** `docs/superpowers/specs/2026-11-22-autoupdate-design.md`

**Execution (supplied by settings sheet):** subagent-driven-development, one fresh agent per task; coder model `opencode/hy4-flash-free#high` on every implementer dispatch; code kit both; merge to `main`; push and tag `v0.2.2` at the end; autopilot.

**Ruling:** Plan accepted as the execution contract without a human plan gate — the human approved the spec and chose autopilot; cost is that plan-level taste calls are made by the orchestrator.

## Global Constraints

- Windows only. No cross-platform fallbacks and no dependencies beyond `sha2`.
- Both binaries are GUI-subsystem; every child process uses `quantdock::hidden()`.
- API: `https://api.github.com/repos/konqiDAM/QuantDock/releases/latest`, with `User-Agent: QuantDock/<version>` and `Accept: application/vnd.github+json`.
- Release assets must be exactly `quantdock.exe` and `monitor.exe`, each with a `sha256:` digest. No third release asset and no installer.
- Version floor = persisted `updates.last_installed_version` when it parses, else `env!("CARGO_PKG_VERSION")`. A tag is valid only as `v` + three decimal segments.
- Staging root: `%APPDATA%\QuantDock\updates\pending\`; markers live in `%APPDATA%\QuantDock\updates\`, never in `pending\`.
- Settings stay draft-based. A malformed `settings.toml` is never overwritten; automatic writes are memory-only when `cfg_err` is set.
- A binary under `target\debug` or `target\release` refuses to update itself.
- Docs ship with code: `README.md`, `INSTALL.md`, `AGENTS.md` and the spec.
- Verify with `cargo test --all-targets` and `cargo build --release`.

## Review Focus

- GitHub returns a release with a missing/wrong asset or no digest → `parse_release` errors and nothing is downloaded. Test: `parse_release_rejects_wrong_assets`.
- A download is corrupted or tampered with → staging rejects the hash, writes no final files, leaves no `.part`. Test: `stage_rejects_bad_digest_and_cleans_up`.
- `settings.toml` is malformed when a snooze or rollback wants to write → the file is untouched and no defaults are serialized. Test: `auto_write_updates_never_overwrites_malformed_file`.
- The monitor exe fails to move during the swap → the script restores backups before relaunch, no half-updated install. Test: `script_checks_every_move_and_rolls_back`.
- A rollback writes an ok marker for the failed tag → failed wins, installed tag is cleared, floor falls back. Test: `apply_markers_failed_wins_and_clears_floor`.

---

### Task 1: Updates settings table

**Files:**
- Modify: `src/monitor_config.rs`
- Test: `src/monitor_config.rs` (`mod tests`)

**Interfaces:**
- Produces: `pub struct UpdatesSettings { pub check_automatically: bool, pub snoozed_until: Option<u64>, pub last_failed_version: Option<String>, pub last_failed_at: Option<u64>, pub last_installed_version: Option<String> }`; `Config.updates: UpdatesSettings`; `Config::defaults()` sets `check_automatically = true`, all other fields `None`. `last_failed_at` persists the failure time so the 24 h retry survives restarts (Ruling: Q1 reversed — an in-memory `Instant` restarts on every launch and suppresses forever; cost is one more defaulted field).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn updates_defaults_are_on_and_empty() { /* assert Config::defaults().updates.check_automatically; others none */ }
#[test] fn updates_round_trip_through_toml() { /* set all five fields, save, load_existing, assert equality */ }
#[test] fn old_settings_without_updates_load() { /* TOML with no [updates] yields defaults */ }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets updates`
Expected: FAIL on unresolved `UpdatesSettings` / `cfg.updates`.

- [ ] **Step 3: Add `UpdatesSettings` with `#[derive(Debug, Clone, Serialize, Deserialize)]`, `#[serde(default)]`, implement `Default`, add `pub updates` to `Config` and to `Config::defaults()`**

Follow the existing `Config` default/serde pattern; do not change `Config::save`.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets updates`
Expected: PASS (plus existing config tests).

- [ ] **Step 5: Commit**

`git add src/monitor_config.rs && git commit -m "feat(config): add updates settings table"`

---

### Task 2: Release parsing, version floor, install paths

**Files:**
- Create: `src/update.rs`
- Modify: `src/lib.rs`, `Cargo.toml`
- Test: `src/update.rs`

**Interfaces:**
- Produces:
  - `pub const RELEASES_API: &str`
  - `pub struct Asset { pub name: String, pub url: String, pub sha256: String }`
  - `pub struct Release { pub tag: String, pub notes: String, pub assets: Vec<Asset> }`
  - `pub fn parse_release(json: &str) -> Result<Release, String>`
  - `pub fn valid_tag(tag: &str) -> bool` (`v` + exactly three decimal segments)
  - `pub fn version_is_newer(tag: &str, floor: &str) -> bool` (tag via `valid_tag`; floor allows optional `v`)
  - `pub fn version_floor(last_installed: Option<&str>) -> String`
  - `pub fn is_dev_path(path: &Path) -> bool`
  - `pub fn install_dir() -> Result<PathBuf, String>`
  - `pub fn writable(dir: &Path) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn parse_release_reads_both_assets_and_digests()
#[test] fn parse_release_rejects_wrong_assets()          // Review Focus
#[test] fn parse_release_rejects_missing_digest()
#[test] fn version_compare_grammar_and_order()           // v0.3.0 > 0.2.0; v0.3, v0.3.1.2, 0.3.0 unparseable
#[test] fn version_floor_prefers_installed_tag()          // Some("v0.2.1") with CARGO 0.2.0
#[test] fn is_dev_path_detects_target_profiles()          // C:\x\target\release\quantdock.exe
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets update`
Expected: FAIL, module does not exist.

- [ ] **Step 3: Add `sha2 = "0.10"` to `Cargo.toml`, `pub mod update;` to `src/lib.rs`, implement the interfaces**

`parse_release` uses `serde_json::Value`; require exactly the two names, `browser_download_url`, and `digest` beginning `sha256:`. `valid_tag` enforces the `v` prefix for tags only; `version_is_newer` parses three `u32`s from the tag and an optional-`v` floor. `install_dir()` returns `current_exe().parent()` and errors on `is_dev_path`.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets update`
Expected: PASS.

- [ ] **Step 5: Commit**

`git add Cargo.toml Cargo.lock src/lib.rs src/update.rs && git commit -m "feat(update): parse releases and version floor"`

---

### Task 3: Verified staged download and launch sweep

**Files:**
- Modify: `src/update.rs`, `src/hf.rs` (make `download_agent()` public)
- Test: `src/update.rs`

**Interfaces:**
- Consumes: `hf::download_agent()` (15 s connect / 60 s per-read timeouts). Ruling: reusing the whole `pump()` would require refactoring `hf`'s `Progress` channel; reuse the agent and keep a bounded read loop. Cost: a small duplicated loop.
- Produces:
  - `pub fn pending_dir() -> PathBuf`
  - `pub fn stage(release: &Release, pending: &Path) -> Result<(), String>`
  - `pub fn staged_complete(pending: &Path, tag: &str) -> bool`
  - `pub fn sweep_pending(pending: &Path, floor: &str) -> bool`

- [ ] **Step 1: Write the failing tests, each with its own temp directory**

```rust
#[test] fn stage_writes_both_files_and_marker_last()   // use known bytes + their sha256 digests
#[test] fn stage_rejects_bad_digest_and_cleans_up()    // Review Focus
#[test] fn staged_complete_requires_marker_and_both_exes()
#[test] fn sweep_keeps_newer_complete_set_and_drops_others()
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets stage`
Expected: FAIL on unresolved functions.

- [ ] **Step 3: Implement the interfaces**

Tests build a `std::net::TcpListener` loopback server and put its URL in the
test `Release`, so no test needs the network. Stream through the agent from
`hf::download_agent()` with bounded reads, hash bytes with `sha2::Sha256`,
write `<name>.part`, verify both before renaming either, write `version.txt` last. Cleanup every created file on any error. `sweep_pending` deletes everything when the marker is missing/unparseable/not newer than `floor` or either exe is missing; returns `true` only when a usable set remains.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets stage`
Expected: PASS.

- [ ] **Step 5: Commit**

`git add src/update.rs src/hf.rs && git commit -m "feat(update): verified staging and sweep"`

---

### Task 4: Markers and swap-script generation

**Files:**
- Modify: `src/update.rs`
- Test: `src/update.rs`

**Interfaces:**
- Produces:
  - `pub fn markers_dir() -> PathBuf`
  - `pub struct Markers { pub ok: Option<String>, pub failed: Option<(String, String)> }`
  - `pub fn read_markers(dir: &Path) -> Markers`
  - `pub fn clear_markers(dir: &Path)`
  - `pub fn write_ok(dir: &Path, tag: &str) -> Result<(), String>`
  - `pub fn write_failed(dir: &Path, tag: &str, reason: &str) -> Result<(), String>`
  - `pub fn read_update_error(dir: &Path) -> Option<String>`
  - `pub fn clear_update_error(dir: &Path)`
  - `pub fn apply_markers(updates: &mut UpdatesSettings, markers: &Markers, now_secs: u64)` (sets `last_failed_at` when the failed marker is applied)
  - `pub fn generate_script(install: &Path, pending: &Path, markers: &Path, pid: u32, monitor_pid: Option<u32>) -> String`
  - `pub fn spawn_script(script: &Path) -> Result<(), String>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn read_markers_reads_both_files()
#[test] fn update_error_is_read_then_cleared_on_dismiss()
#[test] fn apply_markers_failed_wins_and_clears_floor() // Review Focus: failed tag == installed tag
#[test] fn apply_markers_ok_sets_installed_and_clears_failed()
#[test] fn script_checks_every_move_and_rolls_back()    // Review Focus
#[test] fn script_writes_ok_marker_before_start()
#[test] fn script_aborts_when_overlay_pid_is_given()    // spec step 1: no swap, error written, old exe relaunched
#[test] fn script_writes_update_error_on_failure()      // acceptance 11
#[test] fn spawn_script_uses_hidden_nonblocking_cmd()   // assert hidden() + .spawn(), no .status()/.wait()
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets markers`
Expected: FAIL on unresolved functions.

- [ ] **Step 3: Implement the interfaces**

`read_markers` tolerates missing files. `apply_markers` applies `ok` only when `failed` is absent and clears `last_installed_version` when it equals the failed tag; applying `failed` sets `last_failed_at = now_secs`. `generate_script` returns `.cmd` text with literal paths and PID, 60 s PID wait, a script-side overlay abort using `tasklist /FI "PID eq <monitor_pid>"` when `monitor_pid` is `Some` (the exact PID avoids matching unrelated `monitor.exe` processes); on abort write `update-error.txt` and relaunch the old `quantdock.exe` so the user is not left with no app (Ruling Q1), then 2 s settle, `if errorlevel` after each rename/move, rollback before relaunch, `update-ok.txt` written before `Start-Process -PassThru`, `Get-Process -Id` with `$p.HasExited`, `.old` deletion on success, and `update-error.txt` written on every failure or rollback. `spawn_script` runs `cmd /C <script>` through `quantdock::hidden()` with a non-blocking `.spawn()`, dropping the child handle before the manager quits; it must never call `.status()` or `.wait()`, because the script waits for the manager's PID.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets markers`
Expected: PASS.

- [ ] **Step 5: Commit**

`git add src/update.rs && git commit -m "feat(update): markers and swap script"`

---

### Task 5a: Update state, labels and safe settings writes

**Files:**
- Modify: `src/bin/quantdock.rs`
- Test: `src/bin/quantdock.rs` (`mod tests`)

**Interfaces:**
- Consumes: `update::UpdatesSettings` from Task 1.
- Produces:
  - `enum UpdateState { Idle, Checking, Available { version: String, notes: String }, Staged { version: String }, Error(String, Box<UpdateState>) }`
  - `enum UpdateMsg { Available(Release), NoUpdate, Failed(String), Staged(String) }`
  - `fn update_label(state: &UpdateState) -> String`
  - `fn update_tooltip(state: &UpdateState, base: &str) -> String`
  - `fn reduce_update(state: UpdateState, msg: UpdateMsg) -> UpdateState`
  - `fn dismiss_error(state: UpdateState) -> UpdateState` (returns the pending state held by `Error`)
  - `fn auto_write_updates(cfg: &mut Config, path: &Path, cfg_err: Option<&str>, disk_changed: &mut bool, watcher: &mut Watcher, mutate: impl FnOnce(&mut UpdatesSettings)) -> Result<bool, String>`
  - `fn needs_show_window(user_initiated: bool, hidden: bool) -> bool` (true only for a user click while hidden; decides whether `show_window` is needed, never whether the prompt opens)

The split from the tray task is deliberate: these are pure additions and compile alone.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn update_labels_cover_all_five_states()
#[test] fn update_tooltip_appends_error_reason()
#[test] fn reduce_update_handles_check_error_and_dismiss()
#[test] fn dismiss_error_restores_pending_available()        // F10
#[test] fn auto_write_updates_never_overwrites_malformed_file() // Review Focus
#[test] fn auto_write_updates_does_not_flush_unrelated_draft()
#[test] fn auto_write_updates_resets_disk_changed_and_watcher()
#[test] fn detection_does_not_reveal_hidden_window()             // acceptance 12
#[test] fn visible_click_still_opens_prompt()                    // needs_show_window false must not gate the prompt
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets update`
Expected: FAIL on unresolved symbols.

- [ ] **Step 3: Implement the interfaces**

`Error` carries the state it masks; `dismiss_error` returns that pending state
so a release found during an error is not lost.

`auto_write_updates` mutates the draft in memory, but loads a fresh `Config`
from `path`, changes only its `updates` table and saves that, so unrelated
draft edits are never flushed. It resets `disk_changed` and consumes the
`Watcher`'s mtime change after the write.

Add the five labels (`Check for updates`, `Checking for updates…`, `Update available — vX`, `Update ready — restart`, `Update failed — see QuantDock`) and precedence `Error > Staged > Available > Checking > Idle`. With `cfg_err` set, `auto_write_updates` returns `Ok(false)` and touches no file.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets update`
Expected: PASS.

- [ ] **Step 5: Commit**

`git add src/bin/quantdock.rs && git commit -m "feat(update): state, labels and safe settings writes"`

---

### Task 5b: Tray menu and dynamic signals

**Files:**
- Modify: `src/bin/quantdock.rs`
- Test: `src/bin/quantdock.rs` (`mod tests`)

**Interfaces:**
- Consumes: `UpdateState`, `update_label`, `update_tooltip` from Task 5a.
- Produces: `const TRAY_UPDATE: &str`, `tray_menu_items(monitor_up: bool) -> [(&'static str, &'static str); 5]`, `fn tray_labels(state: &UpdateState, monitor_up: bool) -> [(&'static str, String); 5]`, `Manager::sync_tray_update(&mut self)` (reads `self.update_state`, stores `tray_update_label: Option<String>` and only pushes changed labels).

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn tray_menu_has_five_items()   // replace tray_menu_has_the_four_items
#[test] fn tray_update_label_follows_state()              // calls tray_labels; no Manager needed
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets tray_menu`
Expected: FAIL on the four-item assertion.

- [ ] **Step 3: Implement the tray rewiring**

`make_tray` returns `(TrayIcon, MenuItem, MenuItem)`; `Manager::new` stores the
second handle as `tray_update`; `sync_tray_update` rewrites its label. Clear
`tray_update` before `_tray` in `ExitStage::Quit`, and route `TRAY_UPDATE` in
`drain_tray`.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets tray`
Expected: PASS.

- [ ] **Step 5: Commit**

`git add src/bin/quantdock.rs && git commit -m "feat(update): five-item tray menu and live labels"`

---

### Task 6: Background checks and launch state

**Files:**
- Modify: `src/update.rs`, `src/bin/quantdock.rs`
- Test: `src/update.rs`, `src/bin/quantdock.rs`

**Interfaces:**
- Produces:
  - `pub fn snoozed(snoozed_until: Option<u64>, now: u64) -> bool` (true while the 24 h snooze is in the future)
  - `pub fn auto_check_due(enabled: bool, manual: bool, last: Instant, now: Instant) -> bool` (manual always true; automatic true only when enabled and 24 h elapsed)
  - `pub fn failed_tag_expired(last_failed_at: Option<u64>, now_secs: u64) -> bool` (true 24 h after the persisted failure time; the automatic cycle then clears `last_failed_version` and `last_failed_at`)
  - `pub fn should_offer(tag: &str, floor: &str, suppressed: Option<&str>, snoozed: bool, manual: bool) -> bool`
  - `pub fn check_latest_at(url: &str, floor: &str, suppressed: Option<&str>, snoozed: bool, manual: bool) -> Result<Option<Release>, String>`
  - `pub fn check_latest(floor: &str, suppressed: Option<&str>, snoozed: bool, manual: bool) -> Result<Option<Release>, String>` (calls `check_latest_at(RELEASES_API, ..)`)
  - `pub fn should_report_check_failure(manual: bool) -> bool`
  - `pub fn launch_update_state(updates: &mut UpdatesSettings, markers: &Markers, floor: &str, now_secs: u64) -> Option<String>` (pure: mutates only the marker fields — failed wins, sets `last_failed_at`, clears installed tag on match — never touches `check_automatically` or `snoozed_until`; returns the staged tag that survives the sweep)
  - `Manager::start_update_check(&mut self, manual: bool)`, `Manager::drain_update(&mut self)`, `Manager::apply_launch_update_state(&mut self)`

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn snooze_is_active_until_expiry()                     // Q3: survives restart via persisted timestamp
#[test] fn should_offer_honours_floor_suppression_snooze_and_manual()
#[test] fn background_checks_skip_when_disabled()              // calls auto_check_due
#[test] fn launch_check_is_due_with_stale_timestamp()          // auto_check_due(true, false, stale, now)
#[test] fn failed_tag_expires_after_twenty_four_hours()        // Q1: spec's automatic retry
#[test] fn should_report_check_failure_only_for_manual()   // acceptance 7
#[test] fn check_latest_sends_required_headers()           // loopback server on a port from std
#[test] fn launch_state_consumes_markers_before_sweep()       // calls launch_update_state with a base table; no Manager, no %APPDATA%
#[test] fn launch_state_preserves_snooze_and_toggle()         // Y1: marker fields only
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets should_offer`
Expected: FAIL on unresolved functions.

- [ ] **Step 3: Implement the interfaces**

`check_latest_at` issues the ureq GET with the two required headers and returns `parse_release` + `should_offer`; the loopback test calls it with the local URL. Manager stores `update_state`, `last_update_check` (initialised to `Instant::now() - Duration::from_secs(24 * 3600)`, matching the existing `last_poll` trick, so the launch check is due), `update_busy`, `update_rx`; checks at launch and every 24 h only when `cfg.updates.check_automatically` is true, manual checks bypass suppression and snooze, background failures are quiet. When `failed_tag_expired(updates.last_failed_at, now)` is true the automatic cycle clears `last_failed_version` and `last_failed_at` through `auto_write_updates` before checking. A staged, already-approved set is still applied at launch when automatic checks are off (Ruling Q2: disabling future checks must not cancel an approved update; cost: staged updates remain applicable). At launch, `Manager::apply_launch_update_state` calls `auto_write_updates` with a closure that invokes `launch_update_state(&mut updates, markers, floor, now_secs)`, then clears the markers **only when the write returned `Ok(true)`** — with `cfg_err` set the marker stays for the next launch (Ruling Q1: the spec's "deletes both" applies to a successful consumption; cost is one retry). The launch then sweeps with the floor, sets `Staged` from a kept set, and starts the first check. There is no separate `apply_markers` call in the launch path; the pure helper mutating an existing table is the only mark-consumer, so the snooze and toggle are never reset (Ruling Q2).

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

`git add src/update.rs src/bin/quantdock.rs && git commit -m "feat(update): background checks and launch state"`

---

### Task 7: Update UI, actions and exit integration

**Files:**
- Modify: `src/bin/quantdock.rs`
- Test: `src/bin/quantdock.rs`

**Interfaces:**
- Consumes: `UpdateState`, `UpdateMsg`, `auto_write_updates`, `spawn_script` from earlier tasks.
- Ruling: with the tray up, X hides the window; the spec's "closing normally" means tray Exit. Only tray Exit or the `Staged` tray item enters `ApplyUpdate`. Cost: the user must use one of those two paths.
- Produces:
  - `ExitStage::ApplyUpdate`, `const OVERLAY_CLOSE_DEADLINE: Duration = Duration::from_secs(5)`
  - `fn overlay_wait_done(started: Instant, now: Instant) -> bool` (elapsed >= 5 s)
  - `fn take_monitor_pid(monitor_pid: &mut Option<u32>, update_monitor_pid: &mut Option<u32>)` (pure capture used by `advance_exit`, testable without a `Manager`)
  - `Manager::monitor_pid: Option<u32>`, set from the spawned child in `toggle_monitor()` and cleared on the user-close branch of `toggle_monitor()` and in `ExitStage::Quit`; `advance_exit`'s CloseMonitor arm calls `take_monitor_pid` before `close_monitor()`, and `ApplyUpdate` passes the captured `update_monitor_pid` to `generate_script` (Ruling Q2: keep the spec's script-side PID guard; fix the lifecycle and clear stale PIDs).
  - `fn update_preconditions(writable_install: bool, overlay_still_running: bool, staged_ok: bool) -> bool` // true fails
  - `fn exit_stage_for(state: &UpdateState) -> Option<ExitStage>` (only `Staged` returns `ApplyUpdate`)
  - `fn exit_stage_after_preconditions(ok: bool) -> Option<ExitStage>` (`None` cancels the quit, `Some(Quit)` succeeds)
  - `fn prompt_actions(staged: bool) -> [&'static str; 2]`
  - `Manager::update_prompt_ui(&mut self, ctx: &egui::Context)`, `Manager::update_error_ui(&mut self, ctx: &egui::Context)`, `Manager::start_update_download(&mut self)`, `Manager::later(&mut self)`, `Manager::apply_staged_update(&mut self) -> bool`

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn overlay_wait_done_only_after_five_seconds()       // F1
#[test] fn take_monitor_pid_captures_before_clearing()        // P1/NEW-2: pure helper, no Manager
#[test] fn update_preconditions_require_all_three()
#[test] fn staged_update_adds_apply_stage_to_exit()      // calls exit_stage_for
#[test] fn plain_exit_still_quits_without_staged_set()   // exit_stage_for -> Quit
#[test] fn update_prompt_actions_are_download_and_later() // calls prompt_actions
#[test] fn later_snooze_survives_draft_return()           // acceptance 6: auto_write_updates + Config::load_existing, no Manager
#[test] fn failed_precondition_cancels_the_quit()         // acceptance 10: exit_stage_after_preconditions(false) -> None
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets preconditions`
Expected: FAIL on unresolved symbols.

- [ ] **Step 3: Implement the interfaces and wire the UI**

Settings Manager section gains the automatic-checks checkbox and **Check now**. The prompt window shows version and notes with **Download & restart** / **Later**; **Later** writes a 24 h snooze via `auto_write_updates`. Before staging and again before the script spawn, call `update::install_dir()` and `update::writable()`; a dev path or read-only folder reports a notice and stops. At launch read `update_error.txt` via `read_update_error`; the dismissible notice
shows it, appends its reason to `tooltip_text`, calls `clear_update_error` only
on dismissal, and clearing it returns `Idle`. The CloseMonitor arm sets `exit_stage = exit_stage_for(&self.update_state).or(Some(ExitStage::Quit))`, so a plain Exit with nothing staged still quits. Only `ApplyUpdate` polls: `close_monitor()` posts `WM_CLOSE`, so this stage polls `monitor_running()` with `ctx.request_repaint_after(100 ms)` until `overlay_wait_done` (5 s), recording `overlay_close_at` in `Manager`. Only after the overlay is gone (or the deadline passes) run `update_preconditions`; on failure cancel the quit (`exit_stage = None`), restart the overlay and leave a tray-only notice (Ruling Q1: consistent with the hidden-to-tray ruling; cost is the notice can be missed until the tray is clicked), on success spawn the script, then quit. X remains a pure hide; only tray Exit and the `Staged` item enter this path (Ruling Q2). Automatic `Available` detection must never call `show_window`; the `Available` tray click opens the prompt unconditionally and calls `needs_show_window(true, hidden)` only to decide whether the viewport also needs revealing. `Checking` is a no-op; `Staged` starts the quit.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets`
Expected: PASS. Also manually verify acceptance 12 with `start_hidden_to_tray = true`: a background check must change only the tray label and must not reveal the window.

- [ ] **Step 5: Commit**

`git add src/bin/quantdock.rs && git commit -m "feat(update): prompt, settings and exit integration"`

---

### Task 8: Release CI and documentation

**Files:**
- Modify: `.github/workflows/release.yml`, `README.md`, `INSTALL.md`, `AGENTS.md`, `src/update.rs` (tests only)
- Reference (read-only): the spec — acceptance 8 is verified through tray Exit, which is this app's "closing normally"; no spec text changes because it is frozen.
- Test: `src/update.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing new.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn release_workflow_checks_tag_against_version()
#[test] fn docs_describe_update_flow_and_manual_fallback()
```

Both use `include_str!` over the workflow/README/INSTALL paths and assert the exact documented strings (`github.ref_name`, `cargo metadata`, `Check for updates`, `manual download`).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets release_workflow`
Expected: FAIL on missing strings.

- [ ] **Step 3: Make the tests pass**

Bump `Cargo.toml` to `version = "0.2.2"` so the release tag matches (Ruling: the settings sheet chose v0.2.2; cost is the published version advances). The self-fulfilling version-literal test was dropped; the workflow step is the guard. Add a release workflow step that fails unless `github.ref_name` equals `v` plus `cargo metadata --no-deps --format-version 1` package version (the shipped `v0.2.1` tag has Cargo version `0.2.0`). The release workflow's Windows test step also runs `cargo test --all-targets script -- --ignored` (Ruling Q2: CI runs the swap gate on `windows-latest`; cost is a few seconds of CI time). README documents the update flow, the writable-folder requirement and that manual download still works; INSTALL notes the same; AGENTS.md indexes `src/update.rs`, the new spec and the shipped feature, and its test-count line and `--ignored` note are updated to the new totals.

- [ ] **Step 4: Run tests and release build**

Run: `cargo test --all-targets && cargo build --release`
Expected: PASS; both exes build. Acceptance 8 is exercised through tray Exit (or the `Staged` tray item); X only hides the window while the tray exists.

- [ ] **Step 5: Commit**

`git add Cargo.toml Cargo.lock .github/workflows/release.yml README.md INSTALL.md AGENTS.md src/update.rs && git commit -m "chore(release): bump version, check tag and document updates"`

---

### Task 9: End-to-end swap verification in temp folders

**Files:**
- Test: `src/update.rs` (`mod tests`, ignored by default so it can be run explicitly on Windows)
- No product code changes expected.

**Interfaces:**
- Consumes: `generate_script`, `read_markers`, `staged_complete` from Tasks 3/4.

- [ ] **Step 1: Write the failing end-to-end tests**

```rust
#[test]
#[ignore = "runs a generated .cmd against temp folders"]
fn script_swaps_both_exes_and_writes_ok_marker() {
    // temp install with dummy quantdock.exe/monitor.exe, temp pending with new ones + version.txt,
    // temp markers dir; pass the PID of a short-lived child that has already exited;
    // write a dummy "new quantdock.exe" that stays alive past the 15 s poll
    // (e.g. ping -n 20), and kill it in teardown; run cmd /C script;
    // assert both exes replaced, .old/version.txt gone, ok marker holds the tag.
}
#[test]
#[ignore = "runs a generated .cmd against temp folders"]
fn script_rolls_back_and_writes_failed_marker() {
    // dummy new quantdock.exe exits non-zero; assert old exes restored,
    // failed marker holds the tag and a reason, update-error.txt exists.
}
#[test]
#[ignore = "runs a generated .cmd against temp folders"]
fn script_rolls_back_when_new_process_vanishes() {
    // dummy new quantdock.exe exits 0 immediately; $p.HasExited must take the
    // rollback branch, not be read as success.
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --all-targets script_swaps -- --ignored`
Expected: FAIL until the script writer is complete.

- [ ] **Step 3: Fix only what the end-to-end run exposes**

Adjust `generate_script` quoting, `errorlevel` checks or marker writes; do not change the spec.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets script -- --ignored && cargo test --all-targets`
Expected: PASS for both ignored tests and the whole suite. Acceptance 4/8/10/13 are verified here; a live run from `C:\Tools\QuantDock` remains a manual follow-up after the release (Ruling Q1: no released build exists before the tag, so the automated temp-folder run is the gate).

- [ ] **Step 5: Commit**

`git add src/update.rs && git commit -m "test(update): end-to-end script swap and rollback"`

## Plan review log

- Rounds 1–2, Beam Free (max): 8 findings then CLEAN. Fixed: UpdateMsg defined too
  late, missing version bump, stage test seam, update-error helpers, tray
  destructuring/test, parameter naming, quiet/hidden tests, task-size ruling
  later reversed.
- Rounds 1–3, Hy4 flash (high): 9 + 2 findings then CLEAN. Fixed: non-blocking
  spawn, script error writes, X-versus-tray-Exit clarification, URL parameter,
  fresh-config writes, tag grammar, watcher re-baseline, `tray_update` field,
  hidden-window test, spec read-only reference, click-flow gate.
- Rounds 1–7, DeepSeek V4.1 Flash: 10 + 5 + 3 + 3 + 3 + 2 + 2 findings then
  CLEAN. Fixed: overlay polling, toggle/snooze readers, script execution test,
  pure launch seam, task split, workflow test, error state, temp-test
  liveness, pure Manager seams, failed-tag expiry, script PID guard lifecycle,
  tray signature, plain Exit, launch due check, launch-table clobber and
  marker retention.
- Totals: 2 + 3 + 7 = 12 rounds, 47 findings, all fixed. No substitutions.
- Final clean verdict carried two coverage questions, both ruled "add the
  tests": `later_snooze_survives_draft_return` (acceptance 6) and
  `failed_precondition_cancels_the_quit` (acceptance 10).
