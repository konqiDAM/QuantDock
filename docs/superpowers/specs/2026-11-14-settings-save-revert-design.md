# Settings: explicit Save and Return

Date: 2026-11-14
Status: awaiting approval
Pairs with: `2026-11-14-custom-sensors-design.md`

## Intent

Today the Settings tab writes `settings.toml` 300 ms after every change. That is
convenient but unforgiving: there is no moment where you can look at a changed overlay,
decide you preferred it, and put it back.

## Behaviour

- Every control in Settings edits a **draft** copy of the config, in memory. Nothing is
  written until **Save**.
- The live preview always shows the draft, so you can judge a layout before committing.
- **Save** writes `settings.toml` (atomically, as today) and clears the dirty state. The
  overlay picks it up within a second via its own watcher.
- **Return** discards the draft, reloads the config from disk, and switches to the Models
  tab. If there is nothing to discard it just switches tabs — no dialog for a no-op.
- With unsaved changes, a marker appears next to the heading: `Settings ●` (and a short
  "unsaved" hint). No modal.
- Closing the app with unsaved changes loses them; the marker is the warning.
- When the draft is clean, an external edit to `settings.toml` still reloads (the existing
  watcher). While the draft is dirty, the file is left alone and the UI shows that it
  changed on disk, so a Return cannot silently resurrect stale values.

## Why not keep auto-save

Auto-save and "return to the previous state" are contradictory: by the time you want to
go back, the file already changed. An explicit Save is the only way to make Return mean
something.

## Cost

- One extra click after each edit session.
- The overlay previews changes on Save rather than instantly (the in-app preview is
  instant, so tuning a layout still feels live).

## Review focus

- Save with an unreadable file on disk: writing must still be atomic and leave a valid
  file.
- Return with a dirty draft must not write anything.
- Switching to Models with a dirty draft keeps the draft until Save or Return; the tab
  badge still shows the dot.
- The overlay must never see a half-written file (atomic rename already guarantees this).
