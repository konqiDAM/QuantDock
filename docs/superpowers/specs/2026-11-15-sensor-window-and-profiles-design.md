# Sensor manager window, profiles, and left-aligned text

Date: 2026-11-15
Status: approved
Builds on: `2026-11-14-custom-sensors-design.md`, `2026-11-15-log-console-sensor-polish-design.md`

## Intent

Replace the inline sensor picker with one **Configure available sensors** window that
lists every field QuantDock can show, built-in and LibreHardwareMonitor, and lets the
user check or uncheck it. Checked fields appear on the overlay and get the same
label/style/warning/critical configuration as built-in fields. Add ten profile slots so
a complete overlay setup can be saved and loaded. Loading a profile while the current
layout is not saved to any slot asks for confirmation; the confirmation only offers
**Load anyway** and **Cancel**.

Also make the manager's body text left-aligned at every window width. Action groups may
stay right-aligned; prose, headings, cards, modal text and the preview must not be
centred.

## Current state

- Settings has a row builder (`rows_ui`), a field table for built-in fields only
  (`fields_ui`), an inline `Browse sensors` picker, and a custom-sensor label/Remove list.
- `field_def(id)` is the built-in vocabulary; the field table skips every `lhm:` id, so
  custom sensors have no style or threshold controls.
- `monitor_panel::draw_custom` renders custom sensors as text only; it uses the threshold
  colour, but no bar/graph.
- There is no profile storage. `settings.toml` has one `[monitor]`, one `[thresholds]`,
  one `[theme]`, and `[monitor.custom]`.
- The old inline picker is the code path that was present when the app crashed. A panic
  hook now writes `%APPDATA%\QuantDock\crash.log`.

## Goals

1. A single **Configure available sensors** button opens an in-app modal.
2. The modal lists **all** fields: every built-in field and every LHM sensor. Each has a
   checkbox, live value where available, search, and a target row.
3. Checking adds the field to the target row only if the id is absent from **every** row;
   unchecking removes it from every row. Stored labels/styles/thresholds survive
   unchecking for built-ins and custom sensors, so re-checking restores them.
4. Custom sensors get regular per-field config: label, style (text/bar), warn, crit,
   direction, and a `(missing)` marker. Bar is available only for `/load/` sensors.
5. Ten profile slots between Rows and Fields. A slot stores the complete overlay setup:
   `monitor`, `thresholds` and `theme` — rows, custom sensors, field styles, thresholds,
   theme, width, interval, opacity, corner radius, font, labels, always-on-top, close
   button, position, margin, graph samples and title. `sensors` stays out.
6. Loading a profile when the current setup matches no saved slot opens a confirmation
   modal with **Load anyway** and **Cancel** only. Load anyway also discards unsaved
   overlay edits in the draft, and the modal says so.
7. Manager body text is left-aligned; right alignment is reserved for action clusters.

## Non-goals

- Graph/sparkline styles for custom sensors. That needs custom history storage; text and
  bar only for now.
- Separate OS windows for the sensor manager or profiles. Both are in-app modals.
- Putting the LHM executable path, sensor interval, enable flag or autostart in a
  profile. Those are machine settings, not layout presets.
- Profile import/export, cloud sharing, or per-model profiles.
- Automatic saving of the current layout into a profile when loading another one. The
  confirmation does not offer that; use Cancel, save to a slot, then Load.
- Changing the built-in field vocabulary.

## Approaches considered

**Sensor manager: in-app modal vs separate viewport vs keep inline.** The modal is
chosen: it can be 900×600, scrollable and searchable, and it shares the Settings draft
without a second window lifecycle. A separate `show_viewport_deferred` window adds focus,
close, always-on-top and state-sync work for no user gain. Keeping the inline list is
what exists and is too small.

**All fields vs LHM-only.** All fields is chosen: the user wants one place to see and
remove everything, including built-ins. The row builder remains for ordering and quick
adds, but it is no longer the only way to add a field.

**Profiles in `settings.toml` vs a separate `profiles.toml`.** Same file is chosen: one
atomic Save, one draft, one Return, and profiles travel with the layout they describe. A
separate file would save profiles independently of the draft and create two sources of
truth.

**Profile contents: layout only vs all overlay settings.** All overlay settings is
chosen. Loading a profile may change the theme, size, position and polling interval.
Machine sensor settings stay out.

**Custom field config: text/bar + thresholds vs full parity.** Text/bar + thresholds is
chosen. Graphs are deferred; bar is enabled only for `/load/` sensors because other units
have no natural 0–100 scale.

## Design

### 1. The Configure available sensors modal

A button in the Fields section, above the table:

```
[ Configure available sensors… ]  12 checked · 310 LHM sensors
```

It opens an `egui::Window` modal (resizable, default 900×600, not collapsible):

- **Header:** search box, `Refresh`, target-row combo (`Add to: Row N`), and a count.
- **Body:** two groups.
  - **Built-in fields** — the `FIELDS` vocabulary, each with a checkbox and its label/id.
  - **LibreHardwareMonitor** — every live sensor with text, value/unit and id on hover,
    each with a checkbox.
  - Stored custom sensors that LHM is not currently reporting appear under
    **Stored / missing** with a `(missing)` marker and remain checkable. The list is the
    union of the live LHM ids and `monitor.custom`, deduplicated by id; live data wins,
    and stored-only ids are marked missing.
- **Checking** a field adds it to the selected row only if the id is absent from **every**
  row; the check scans all rows, not only the target. For a custom sensor, the label is
  the LHM text uppercased and truncated to 12 characters; it stays editable in the custom
  field table.
- **Unchecking** removes the id from every row. It does **not** delete a label, style or
  thresholds for a built-in or a custom field, so re-checking restores them. Only the
  custom `Forget` action deletes stored config.
- Shared helpers `add_field_to_row(cfg, row, id)` and `remove_field_everywhere(cfg, id)`
  are used by the modal, the row builder and the tests; no path may duplicate an id or
  leave it in a row after unchecking.
- **Target row** is clamped when rows are deleted. With no rows, adding is disabled and
  the modal says `Add a row first`.
- **LHM not running:** built-ins and stored custom sensors still work; the LHM group says
  `Start LibreHardwareMonitor, then Refresh`.
- The live list is fetched on open and on `Refresh`, on a worker thread, reusing the
  existing `PickerSensor`/`Reader` path. No polling while the modal is open.

The inline picker and the old custom-sensor label/Remove list are removed. The paste box
moves under the custom field table as **Add by SensorId**, so an unreported SensorId can
still be stored by hand. The `+ field` menu in the row builder stays as a shortcut, but
the modal is the complete view.

### 2. Regular config for custom sensors

Below the built-in field table, a **Custom sensor fields** table lists every stored
`monitor.custom` id (whether or not it is currently in a row), sorted by label then id:

| Field | Style | Warn | Crit | Direction | |
|---|---|---|---|---|---|
| `GPU PACKAGE` (`lhm:/gpu-nvidia/0/power/0`) | text | ☐ | ☐ | above | Forget |

**Resolver.** `FieldDef` is `&'static`; add a small borrowed `FieldView<'a> { id: &'a str,
label: &'a str, kind: FieldKind }` and route `format_value`, `threshold_value`,
`bar_fraction`, `draw_field` and `natural_width` through it. Built-ins convert from
`FieldDef`; custom fields take the config label and infer the kind from the id: `/load/` ->
`Percent`, `/temperature/` -> `Celsius`, `/power/` -> `Watts`, `/clock/` -> `Mhz`,
`/fan/` -> `Rpm`, otherwise `Number` (two decimals). The old `draw_custom` and
`custom_row_width` paths are deleted: `draw_field` and the single width loop handle both
kinds, so auto-width includes a custom bar's width and does not measure every custom
sensor as text.

- Label is editable.
- Style: `text` or `bar`. `bar` is enabled only when the id contains `/load/`; the live
  `%` unit is not persisted, so it is not used to decide eligibility. Non-percent custom
  sensors are text-only. A stored `bar` on a non-`/load/` id is ignored (rendered as text)
  and is not rewritten just by opening Settings.
- Warn/crit use the existing `Threshold` shape and direction; colours flow through the
  existing `color_for`.
- A missing id shows `(missing)` and `--` in the overlay, never a remapped value and never
  a zero bar.
- `Forget` removes the id from every row, `monitor.custom`, `field_style` and
  `thresholds`. It is the only destructive custom action; built-ins have no Forget.

`monitor_panel::draw_field` gains the same style branch as the built-ins: text by default,
the same bar rendering for `/load/` sensors, and the threshold colour. No graph or history
for custom sensors in this version.

### 3. Ten profile slots

A **Profiles** section between Rows and Fields:

```
Profiles
[1] [2] [3] [4] [5]
[6] [7] [8] [9] [10]
Selected: Slot 3 — saved
[ Save current to slot 3 ]  [ Load slot 3 ]
```

- A 5×2 grid of ten numbered buttons. Clicking selects; empty slots show a dash and can
  be saved into. Filled slots show their number in the accent colour. Tooltips say
  `Empty` or `Saved`.
- `Save current to slot N` snapshots the current draft into `cfg.profiles[N]`. Saving into
  an occupied slot asks `Overwrite slot N?` with **Overwrite** / **Cancel**; there is no
  silent overwrite. It marks the Settings draft dirty (main **Save** persists) and clears
  the profile-dirty state for that snapshot.
- `Load slot N` is disabled for empty slots. If the current snapshot equals any saved
  slot, it loads immediately. Otherwise it opens a modal:

```
Load profile 3?
The current layout is not saved to any profile.
This also discards unsaved overlay edits in the draft.

[ Load anyway ]  [ Cancel ]
```

- `Load anyway` replaces the draft's `monitor`, `thresholds` and `theme` with the slot
  and keeps `sensors` untouched. It marks the Settings draft dirty; main **Save** applies
  it to `settings.toml` and the overlay. `Cancel` changes nothing.
- The profile-dirty check compares the current `Profile` (a `PartialEq` snapshot) with
  every filled slot. Exact equality is used: a freshly loaded profile is clean, and any
  edit to an included value is dirty. Editing only the LHM path or sensor interval is not.
- Profiles are stored in `settings.toml` as `profiles: BTreeMap<String, Profile>` with
  keys `"1"`–`"10"`, serialized under `[profiles.1.monitor]`, `[profiles.2.monitor]`,
  and so on (the TOML writer may quote a numeric key as `[profiles."1".monitor]`; the
  parsed key is still `"1"`). There is no bare `[profiles.1]` header because `Profile`
  has only sub-tables. `Profile` is
  `{ monitor: MonitorSettings, thresholds: BTreeMap<String, Threshold>, theme: Theme }`,
  derives `Default`/`PartialEq`, and excludes `sensors`.
  `profile_round_trips_through_toml` pins the shape, including nested
  `[[profiles.1.monitor.rows]]`, `[profiles.1.thresholds.gpu_temp]` and
  `[profiles.1.theme]`.
- The existing config watcher reloads profiles with the rest of the config, but only while
  the Settings draft is clean; a dirty draft ignores external edits and shows the notice.
  `Return` discards unsaved profile saves with the rest of the draft.

### 4. Text alignment

The manager is left-aligned at every width:

- Model-card titles, alias/args labels, Settings headings and prose, modal text, preview
  captions and field tables start at the left edge.
- `Layout::right_to_left` remains only for action clusters: Load, Save/Return, Monitor/
  Refresh, Add/Remove, and the profile action buttons.
- No `Layout::centered` or `Align2::CENTER_CENTER` for body text. The overlay close glyph
  and values drawn inside bars stay centred; they are controls, not prose.
- The sensor modal and the profile confirmation follow the same rule.

A narrow window must wrap or scroll, never centre a label. The row/field tables use the
same left alignment at 520 px as at full width.

Audit every release with:

```
rg -n "Layout::centered|Align2::CENTER_CENTER" src/bin/quantdock.rs src/bin/monitor.rs src/monitor_panel.rs
```

The only allowed matches are the overlay close glyph and values drawn inside bars. Any
other match is body text and must be left-aligned.

### 5. Crash handling

This spec **does not claim to fix an unidentified crash**. It removes the old inline
picker path, adds pure tests for add/remove and a manual modal stress test (open, search,
check, uncheck, change target row, LHM off, empty rows), and keeps the panic hook writing
`%APPDATA%\QuantDock\crash.log` with a backtrace. Before claiming the crash fixed:
reproduce the action, read `crash.log` if present, and test the new modal. If there is no
reproduction, report the crash as unresolved; the new path and log are the evidence for
the next attempt.

## File structure

| File | Change |
|---|---|
| `src/monitor_config.rs` | `Profile` snapshot, `profiles: BTreeMap<String, Profile>` on `Config`, defaults, serialization tests. |
| `src/bin/quantdock.rs` | Configure-sensors modal state/UI; `add_field_to_row`/`remove_field_everywhere`; remove inline picker/custom list; custom field config table; profile grid/save/load/overwrite; confirmation modal; left-align audit. |
| `src/monitor_panel.rs` | `FieldView` resolver, unified `draw_field` for built-ins and custom, style-aware width, custom bar tests. |
| `src/monitor_sample.rs` | No custom history this version. |
| `src/lib.rs` | Panic log already present. |
| `README.md` / `INSTALL.md` | Document the new window, profiles and left alignment. |
| `AGENTS.md` | Code map and current state. |

## Tests

- `profile_round_trips_through_toml` (pins `[profiles.1.monitor]`, nested rows/thresholds/theme)
- `save_profile_snapshots_the_full_overlay_config`
- `applying_a_profile_keeps_sensor_settings`
- `profile_matches_only_its_own_layout` (clean and dirty)
- `sensor_only_edits_do_not_dirty_a_profile`
- `add_field_to_row_scans_every_row`
- `picker_add_does_not_duplicate_a_field_across_rows`
- `remove_field_everywhere_clears_all_rows`
- `unchecking_keeps_style_and_thresholds`
- `unchecking_a_custom_sensor_keeps_its_label`
- `forget_removes_rows_custom_style_and_threshold`
- `custom_kind_infers_from_the_sensor_path`
- `custom_bar_uses_the_percent_scale_and_missing_is_dash`
- `custom_width_matches_a_bar_style`
- `custom_bar_is_only_allowed_for_load_sensors`
- Existing `custom_sensor_contributes_width` covers the natural-width case.
- Overwrite is UI state; it is checked manually in Task 6 of the plan.
- Existing 73 tests stay green; 4 ignored.

## Review focus

- Unchecking a built-in removes it from every row but keeps its style/thresholds, so
  re-checking restores them; no duplicate ids anywhere.
- A custom sensor can be checked twice without duplicate rows; target-row clamping.
- Profile save/load with no slots, all slots full, overwrite confirmation, and a dirty
  draft; Load anyway discards unsaved overlay edits and says so.
- Loading a profile does not change the LHM path, interval or autostart; sensor-only
  edits do not mark a profile dirty.
- Confirmation only has Load anyway/Cancel; Cancel is lossless.
- Left alignment at 520 px; no centred body text in the modal or preview.
- Custom percent bar with a missing value shows `--`, not a zero bar; auto-width accounts
  for the bar.
- The old picker path is gone; panic log remains; no crash is claimed fixed without a
  reproduction.

## What is left to the builder

- The exact sensor modal grouping (flat vs collapsible hardware sections); a flat
  searchable list with a Built-in/LHM split is the default.
- Slot labels: numbers only for now; names can come later.
- Whether `Forget` asks for confirmation; default is a single click with no dialog.
- Exact default width of the custom field table and the modal's minimum size.
