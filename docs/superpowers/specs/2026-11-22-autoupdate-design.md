# QuantDock autoupdate

Date: 2026-11-22. Status: proposed, pending spec review and approval.

## Intent

Stop hand-downloading two `.exe` files. QuantDock checks GitHub for a newer
release in the background, tells the user what is new, and — only after the
user approves — downloads and verifies both executables, swaps them at process
exit and relaunches. The user's goal is convenience: one approval from
notification to a running new version, with both executables kept in sync.

## Goals

- Background check at manager launch and every 24 h while it runs.
- A manual **Check for updates** action that reports result even when idle.
- Prompt with the new version number and release notes; **Download & restart**
  or **Later** (snoozed 24 h).
- Download both release assets (`quantdock.exe`, `monitor.exe`), verify each
  against the SHA-256 digest in the GitHub release API, and stage them before
  anything is replaced.
- Swap at exit using a short-lived hidden script; relaunch the manager; keep
  `.old` backups and auto-restore them if the new manager does not come up.
- An **Automatic update checks** switch in Settings (default on). Off means
  checks happen only on manual request.
- Clear status-bar notices for failures; never overwrite config, models or
  logs, and never leave the install half-updated.

## Non-goals

- No installer, MSI, or third release asset. The install stays two loose exes.
  Reason: would change CI, release notes and the documented manual install.
- No updates for LibreHardwareMonitor, llama-server, models or config. Reason:
  those have separate, deliberately manual flows with different trust rules.
- No silent background install. Reason: the user chose approval before install.
- No prereleases, downgrades or beta channel. `/releases/latest` already
  excludes prereleases; a tag not newer than the version floor is ignored.
- No delta/patch updates. Reason: two small exes make full replacement simpler.
- No cross-platform support. Windows-only, matching the rest of the app.

## Constraints

- Rust 2024, Windows only, egui/eframe 0.37. No new GUI framework.
- `ureq` 3 (with `json`) and `serde_json` are already dependencies. Add only
  `sha2` for digest verification; no version-parsing crate.
- Both binaries are GUI-subsystem: any child process uses `quantdock::hidden()`.
- Manager config lives in `%APPDATA%\QuantDock\settings.toml`, is draft-based
  (Save writes, Return reloads) and parses unknown/missing keys with defaults.
- The manager is normally always in the tray and owns a single-instance mutex;
  the swap must wait for it to exit before relaunching, and the overlay is
  closed by the existing exit state machine before the swap.
- The app is installed as loose exes in one writable folder (README suggests
  `C:\Tools\QuantDock`). If that folder is not writable, the update fails with
  a clear notice and the current install stays untouched.
- CI is `.github/workflows/build.yml` and `release.yml`; tags `v*` publish the
  two exes. Release assets expose SHA-256 `digest` values via the GitHub API.
  `release.yml` gains one check that the tag equals `v` plus the package
  version, because the shipped `v0.2.1` tag already disagrees with its
  `Cargo.toml` (`0.2.0`).
- Build: `cargo build --release`. Tests: `cargo test --all-targets`.
  Docs ship with code: `README.md`, `INSTALL.md`, `AGENTS.md`, and this spec.
- Reviewers: Beam Free (max), Hy4 flash Free (high), DeepSeek V4.1 Flash.

## Reviewers

Selected in Phase 2: Beam Free (max), Hy4 flash Free (high), DeepSeek V4.1 Flash
(roster order #1, #5, #8).

## Approaches considered

1. **Notify only** — a version check and a link. Rejected: still needs a
   browser round trip and two manual downloads, the exact problem to solve.
2. **Manual check + confirm** — no background polling. Rejected: the user
   chose automatic checks with an approval step.
3. **Automatic check + confirmation** (chosen) — checks on a schedule, asks
   before installing. Keeps the convenience goal without silent replacement.
4. **Silent automatic install** — rejected: no user control over what runs.
5. **Swap via a third updater binary** — more robust, but adds a release
   asset and changes CI and install docs. Rejected in favour of a generated
   short-lived script.
6. **Swap via a generated hidden `.cmd`** (chosen) — no extra asset, works
   with the existing `hidden()` helper. Cost: batch-script edge cases around
   quoting and process detection, covered by a focused test of the generator.

## Design

### Version source and comparison

- New module `src/update.rs`, owned by the manager binary through a thin
  re-export in `src/lib.rs` if needed.
- Check endpoint: `https://api.github.com/repos/konqiDAM/QuantDock/releases/latest`
  with `User-Agent: QuantDock/<version>` and `Accept: application/vnd.github+json`.
- Parse `tag_name`, `body`, and `assets[]` fields `name`,
  `browser_download_url`, `digest` using `serde_json`.
- Require exactly the asset names `quantdock.exe` and `monitor.exe` with a
  `sha256:` digest. A malformed or incomplete release yields an error, never
  a partial update.
- Compare a tag like `v0.3.0` to the version floor as three `u32`s. The floor
  is the persisted `updates.last_installed_version` when it parses, otherwise
  `env!("CARGO_PKG_VERSION")`; the installed tag is persisted from the
  `update-ok.txt` marker after a successful swap. This matters because the
  shipped `v0.2.1` tag disagrees with its `Cargo.toml` — without the persisted
  floor, installs would be offered v0.2.1 forever. Installs with no persisted
  tag get one final v0.2.1 offer, then stop.
- A tag must be exactly `v` plus three decimal numeric segments; four-segment,
  short or otherwise malformed tags are unparseable and are ignored in
  background checks and reported on manual checks. No semver crate, no
  pre-release handling.

### State and configuration

- `Config` gains an `updates` table: `check_automatically: bool` (default
  `true`), `snoozed_until: Option<u64>` (Unix seconds),
  `last_failed_version: Option<String>` (the rolled-back tag) and
  `last_installed_version: Option<String>` (the last successfully installed
  tag). All come from `#[serde(default)]` and need no migration.
- Manager-local runtime state: `UpdateState` with `Idle`, `Checking`,
  `Available { version, notes }`, `Staged { version }`, `Error(String)`.
  Network failures during background checks stay quiet in the status bar;
  manual checks always report success or failure.
- State precedence for the tray label is `Error` > `Staged` > `Available` >
  `Checking` > `Idle`. A successful check may update the underlying `Available`
  state, but the `Error` label stays until the notice is dismissed; dismissal
  returns to `Idle` (or reveals the pending `Available`/`Staged` state).
- On rollback the manager persists the failed tag in
  `updates.last_failed_version`. Automatic checks suppress that exact tag until
  the next 24 h cycle (which clears the field) or a manual check (which
  ignores it). Ruling: suppression must survive the restart that rollback
  performs, so it has to be on disk; cost is one config field and a delayed
  retry after a transient failure, avoiding a bad-release loop.
- Every automatic write to the `updates` table (snooze, failed tag, installed
  tag, 24 h clear and marker consumption) changes both the on-disk config and
  the in-memory draft atomically, without flushing unrelated draft edits, so a
  later Save cannot restore a stale value. The write resets `disk_changed`
  the way Save does, so the app's own write is not reported as an external
  change. If `cfg_err` is set (malformed `settings.toml`), automatic writes are
  memory-only: never serialize defaults over the user's recoverable file.
- At launch, a complete staged set whose marker is newer than the version floor
  rehydrates `UpdateState` to `Staged { version }`, so a kept set can be applied
  from the tray instead of being re-downloaded. Background checks still run
  when the manager is hidden or the network is down; failures stay quiet.
- **Later** sets `snoozed_until = now + 24 h` in both the on-disk config and
  the in-memory draft, then saves atomically without writing any other draft
  edits. Neither Return nor a later Save of unrelated settings can discard the
  snooze. The prompt is suppressed while that time is in the future. Ruling:
  immediate persist was chosen because the user's goal is one-click
  convenience; cost is that this one field bypasses the normal Save gesture.
- A detected update never reveals the manager window when `start_hidden_to_tray`
  is on; the tray item's label is the only signal until the user clicks it.
  Ruling: the hidden-to-tray preference wins over prompt visibility; cost is
  that the prompt can be missed until the tray label is noticed.
- On launch, sweep `%APPDATA%\QuantDock\updates\pending\` only when the staged
  set is unusable: no `version.txt` marker, an unparseable marker, a marker
  that is not newer than the version floor, or either staged exe missing.
  A complete staged set newer than the version floor is kept so a failed
  precondition or script spawn can be retried. A successful swap deletes the
  staged files and marker; a rollback discards them too (Ruling: after the new
  exe has failed, re-applying it risks another bad release; cost is a
  re-download).

### Download and verification

- Before staging, probe install-folder writability and refuse with a notice;
  a read-only install must not download ~21 MB only to fail at exit. The same
  probe runs again before the script spawns.
- A binary whose exe path contains `target\<profile>` is a dev build: refuse
  to update it with a clear notice. Ruling: a released update must never
  overwrite the working tree's build output; cost is that update testing needs
  a released exe.
- `stage(release)` streams each asset to
  `%APPDATA%\QuantDock\updates\pending\<name>.part`, hashing bytes as they
  arrive with `sha2`.
- Both files must hash to their API digests before either is renamed to its
  final name in `pending\`. A `version.txt` marker holding the release tag is
  written last, so a partial stage is never mistaken for a complete one. Any
  failure deletes all `.part` and final staged files and leaves the install
  untouched.
- Reuse the project's existing download/cancel conventions: bounded reads,
  no whole-file buffering, cleanup on every error path.

### Prompt and user flow

- Tray menu grows from four to five items; `TRAY_UPDATE` label maps from the
  runtime state: `Idle` → `Check for updates`, `Checking` → `Checking for
  updates…`, `Available` → `Update available — vX`, `Staged` → `Update ready —
  restart`, `Error` → `Update failed — see QuantDock`. Its tooltip carries the
  failure reason is appended to the existing icon tooltip produced by
  `tooltip_text(serving, monitor_up)`, since `tray-icon` has no per-item
  tooltip; this keeps a hidden manager signaled. Clicking it checks
  for updates when idle, is a no-op while checking, reveals the window and
  opens the prompt when available (a user click is not the automatic reveal
  the hidden preference forbids), quits to apply the staged update when ready
  (with the same
  dirty-draft confirm as Exit), and opens the dismissible error notice when
  failed. `make_tray` returns both the
  monitor and update `MenuItem` handles so both labels can be rewritten on
  state changes.
- Settings gains the automatic-checks toggle and a **Check now** button, both
  operating on the existing in-memory draft.
- An egui window shows the new version and release notes with **Download &
  restart**, **Later** and (once staged) a note that the swap happens on exit.
- Approving downloads and stages, sets `Staged`, then follows the existing
  quit path (dirty-draft confirm → cancel download → close overlay → quit).
  Closing QuantDock normally with a staged update applies it too.

### Swap and rollback

Before spawning the script, the manager checks three preconditions. A failure
keeps the staged files, reports a notice, cancels the quit (`exit_stage = None`)
and restarts the overlay, so the user is not left with an unapplied exit:

- Install folder writability: create and delete a probe file in the directory
  returned by `std::env::current_exe().parent()`. Never discover this after
  the UI is gone.
- Overlay exit: after the exit state machine's CloseMonitor step, poll up to
  5 s for a `monitor.exe` launched from the install folder to be gone. If it
  still holds `Local\QuantDockMonitor`, abort the swap.
- Staged set: `version.txt` and both staged exes still exist and the tag is
  newer than the version floor.

The manager then generates a hidden `.cmd` under `%TEMP%` containing literal
paths derived from `std::env::current_exe()` and its own PID. The script:

1. waits for that PID to exit, bounded to 60 s, and aborts if the overlay is
   still running;
2. waits 2 s for Windows to release file locks on the just-exited exe;
3. reads the tag from `version.txt`, then deletes any stale
   `quantdock.exe.old` / `monitor.exe.old` and renames the installed exes to
   those names, checking the exit code after each operation;
4. moves both staged exes into the install folder, checking the exit code after
   each move;
5. on any rename/move failure, restores the backups and starts the old
   `quantdock.exe` without deleting anything else;
6. writes `update-ok.txt` containing the tag read in step 3 **before** starting
   the new exe, so the new process sees the installed tag at its own launch;
   `failed-version.txt` wins over it if rollback follows;
7. starts the new `quantdock.exe` with hidden
   `powershell -NoProfile Start-Process -PassThru`, then polls up to 15 s with
   `Get-Process -Id`; the verdict is `$p.HasExited` (exit code consulted only
   when it has exited), since a healthy GUI process stays running;
8. on success, deletes the staged files, `version.txt` and the `.old` backups;
   if the new process exits or is gone, writes the rollback reason and the tag
   to `failed-version.txt`, deletes `version.txt` and any staged files,
   restores the `.old` files and starts the old `quantdock.exe`. Stale backups
   are removed on success because automatic rollback already covers recovery
   (Ruling: no ~21 MB of dead backups).

The markers live in `%APPDATA%\QuantDock\updates\`, outside `pending\` so the
launch sweep cannot eat them. The next manager launch consumes **both** before
the launch background check, deletes both, and applies `failed-version.txt`
over `update-ok.txt` when both are present (Ruling: failed wins, so a stale ok
marker can never clear a fresh failure). `update-ok.txt` clears
`updates.last_failed_version` and sets `updates.last_installed_version` from
its tag; `failed-version.txt` writes the tag into `last_failed_version` and
clears `last_installed_version` when it equals that tag, so the floor falls
back to `CARGO_PKG_VERSION` and a manual check can offer the release again as
an explicit override (Ruling: a failed release must never raise the floor).
This is the only way the restarted process can know which release
failed (Ruling: the script is the only process alive at rollback time, so it
writes the marker). Any script failure, including rollback, is
also written to `%APPDATA%\QuantDock\update-error.txt`; the next launch
surfaces the message in a dismissible notice (not the 6 s status note) and
clears the file only when the user dismisses it. If command execution itself
fails, the app reports the error and leaves the staged files in place for the
next attempt. No registry changes, no shortcuts, no services.

## Acceptance criteria

1. Release JSON parsing returns both exact assets with their digests; wrong
   names, missing digests and malformed JSON are errors, not partial results.
2. Version comparison accepts `v0.3.0` over `0.2.0`, rejects equal and older
   tags, and treats `v0.3`, `v0.3.1.2` and `0.3.0` as unparseable. A persisted
   `last_installed_version` wins over `CARGO_PKG_VERSION` as the floor, so
   `v0.2.0` is not offered again to an install that recorded `v0.2.1`.
3. Staging verifies each `.part` against its digest before any final file
   appears; the `version.txt` marker is written last; a mismatch removes all
   staged files and reports an error. The launch sweep keeps a staged set only
   when its marker is newer than the version floor and both staged exes exist;
   it deletes every other set. At launch a kept set rehydrates `Staged`, and
   the tray applies it without re-downloading.
4. The generated script contains the real install/staging paths and PID, waits
   for exit, settles 2 s before renaming, checks each rename/move exit code and
   rolls back before relaunching if any fails, reads the tag, writes
   `update-ok.txt` before starting the new exe, starts it with
   `Start-Process -PassThru` and polls `Get-Process -Id` up to 15 s using
   `$p.HasExited`, then deletes staged files, `version.txt` and `.old` backups.
   On failed start it writes `failed-version.txt` and the reason, deletes the
   staged set, restores the backups and relaunches the old exe. The next launch
   consumes both markers before its first check, deletes them, applies the
   failed marker over the ok marker, sets `last_installed_version` from the ok
   marker, and clears that field when the failed marker names the same tag.
5. All four `updates` fields default correctly, serialize to TOML and survive
   draft Save/Return like other settings; every automatic write to the table
   updates disk and draft together, resets `disk_changed`, and is memory-only
   while `cfg_err` is set (the malformed file is never overwritten). A
   persisted failed tag suppresses offers by automatic checks until the next
   cycle, while a manual check ignores the suppression.
6. Tray menu has five items with five state-driven labels including `Checking`,
   a click action for each (`Checking` is a no-op; `Available` reveals the
   window), state precedence, and the error reason appended to `tooltip_text`;
   dismissing an error returns the
   state to `Idle`. `make_tray`
   exposes both menu-item handles. The prompt shows version and release notes;
   **Later** persists a 24 h snooze without a separate Save and is not
   discarded by Return.
7. Background check failures do not spam the status bar; manual checks always
   show a result.
8. A staged update is applied by closing the manager normally; the overlay is
   closed first, and only the two QuantDock executables are replaced.
9. Docs: README documents the update flow and the writable-folder requirement;
   INSTALL notes that manual download still works; AGENTS.md index updated.
   `release.yml` fails a tag whose `v`-prefixed name does not match the package
   version.
10. An unwritable install folder is detected before staging, and a still-running
    overlay or a missing staged set before the script spawns: notice shown,
    staged files kept, quit cancelled and overlay restarted, no download or
    swap attempted.
11. A script failure or rollback writes `update-error.txt`; the next launch
    shows it in a dismissible notice and clears the file only on dismissal,
    and the tray label/tooltip carries the failure state.
12. With `start_hidden_to_tray` on, detecting an update changes only the tray
    label; the manager window stays hidden. Background checks still run when
    hidden or offline and fail quietly.
13. A binary under `target\<profile>` refuses to update with a notice; a
    release install accepts the same flow.

## Open questions

None blocking.

## Review log

- Round 1, Beam Free (max): MAJOR, 8 findings (writability pre-check, overlay
  precondition, tray handle, stale `.old`, snooze persistence, mutex settle,
  click actions, tag grammar). All fixed; Q1 ruled "Later persists immediately".
- Round 2, Beam Free (max): CLEAN.
- Round 1, Hy4 flash (high): MAJOR, 6 findings (staged-set sweep contradiction,
  hidden-mode prompt, snooze overwritten by Save, transient error notice,
  silent rollback, settle placement). All fixed; Q1 ruled "tray label only" for
  hidden managers.
- Round 2, Hy4 flash (high): MAJOR, 2 findings (orphaned marker after rollback,
  hidden error notice). Fixed; Q2 ruled "discard staged set on rollback".
- Round 3, Hy4 flash (high): MINOR, 1 finding (tray state machine). Fixed; Q3
  ruled "no automatic re-offer of a rolled-back release".
- Round 4, Hy4 flash (high): MAJOR, 2 findings (unpersisted failed tag, missing
  click action). Fixed; Q4 ruled "persist `last_failed_version`".
- Round 5, Hy4 flash (high): MAJOR, 1 finding (restarted manager cannot learn
  failed tag). Fixed; Q5 ruled "script writes structured markers".
- Round 6, Hy4 flash (high): MINOR, 2 findings (marker location/ordering, draft
  sync). Fixed; Q6 ruled "consume both markers, failed wins".
- Round 7, Hy4 flash (high): CLEAN.
- Round 1, DeepSeek V4.1 Flash: BLOCKER, 9 findings (version-floor drift,
  partial swap, staged rehydration, malformed config protection, process probe,
  watcher, tooltip, writability timing, dev build). All fixed; Q1 persisted tag
  plus CI check, Q2 keep background checks, Q3 dev builds refuse.
- Round 2, DeepSeek V4.1 Flash: MAJOR, 6 findings (ok-marker timing, staged-set
  rehydration on success, precondition quit, floor wording, hidden click,
  process verdict). All fixed; Q1 ruled "delete `.old` on success".
- Round 3, DeepSeek V4.1 Flash: MAJOR, 2 findings (rollback raises floor, stale
  non-goal). Fixed; Q1 ruled "clear installed tag on matching failed tag".
- Round 4, DeepSeek V4.1 Flash: CLEAN.

Totals: 2 + 7 + 4 rounds; 27 findings, all fixed; 12 rulings. No substitutions.
