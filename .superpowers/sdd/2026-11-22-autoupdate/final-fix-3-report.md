# Final fix 3 report

## Findings fixed

1. Manual update checks now report success: `drain_update` shows "QuantDock is up to date" / "update {tag} available" for manual checks. Added `update_drain_note` (plus aliases) and a test.
2. Tray update click now routes through `effective_update_state`, so a failed item opens the notice instead of starting a check. Added `update_click_action` and a test table.
3. A vanished/incomplete staged set no longer blocks Exit forever: it drops to Idle and continues to Quit. Added `exit_stage_on_failed_preconditions` (plus aliases) and a test.
4. Rollback restore blocks replaced with a goto-label chain to survive `)` in the install path.
5. AGENTS.md test-count line updated to 204 tests.
6. `check_latest_at` rejects a non-version tag with an error before `should_offer`.
7. `later()` skips the disk write when the Settings draft is dirty and keeps the snooze memory-only.

## Tests

cargo test --all-targets: 146 lib + 55 bin passed, 7 ignored.
cargo test --all-targets script -- --ignored: 3 passed.
