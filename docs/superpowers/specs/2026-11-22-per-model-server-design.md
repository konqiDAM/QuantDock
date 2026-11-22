# Per-model `server` key in the model TOML

Date: 2026-11-22. Status: shipped on `main` (2026-11-22).

## Intent

Each model can declare its own llama-server via an optional `server` key in its
per-model TOML. Inkling-2-Mini points at the unsloth patched build (mainline
outputs gibberish); the Qwen models keep no key and fall back to the default
chain. This stops the unsloth exe from being forced on every model.

**Rollout (required, not optional):** the intent does not happen while
`QUANTDOCK_LLAMA` is still set — it is the first link in the default chain, so
every model without a `server` key would still launch unsloth. Before the
feature is considered done:

1. Clear the User-scope `QUANTDOCK_LLAMA` (`HKCU\Environment`).
2. Retire or delete the untracked `run-quantdock-unsloth.cmd` (it sets the
   same value per launch).
3. Restart QuantDock.

## Goals

- `server = "C:/path/to/llama-server.exe"` — optional, per-model TOML only.
- If present and the file exists → use it.
- If present but missing → **Load fails** with a clear error. No silent
  fallback to the wrong build.
- If absent → the existing default chain: `QUANTDOCK_LLAMA` → LM Studio
  backends → `PATH`.

## Non-goals

- No UI field for the server (TOML is hand-edited, like `args`).
- No global `llama_server` key in settings.toml.
- No changes to the default chain.
- No validation of the path at discovery time — a missing path shows the
  error at Load time, not a broken card.

## Constraints / notes

- The `server` value **must be an absolute path**. It is validated with
  `is_file()` against the process CWD, but the eventual `Command::new(&exe)`
  runs with `current_dir(&model.folder)` — a relative value could pass the
  check and then fail at spawn. `resolve_server` rejects a relative path
  explicitly (see Files to change).
- **TOML basic-string escaping:** in a basic string (`"..."`), a backslash
  starts an escape. A path like `C:\Tools\llama.cpp\llama-server.exe` fails
  to parse (the model then disappears from the list — `discover()` has no
  error path), while `C:\temp\bin\t.exe` parses **silently** as a wrong path
  (`\t` is a valid escape). Use forward slashes (`C:/...`) or a single-quoted
  TOML literal string (`'C:\...'`). Do not tell the reader "backslashes always
  error" — they don't.
- **Empty value:** `server = ""` becomes `Some("")` and is rejected by the absolute-path
  check, producing `llama-server path must be absolute: `. Acceptable; documented here so
  a reader does not expect empty to mean "absent".
- **Naming:** `unload()` kills by image name (`llama-server.exe`). A per-model
  exe must keep that name, otherwise Close/Quit will not kill it and the next
  Load hits a busy port 8080.
- **`_runtime` is safe:** it sits inside the models root but has no
  `_runtime.toml`, so `discover()` skips it; `sweep_partials` only deletes
  folders holding nothing but `*.gguf.part`, so the runtime tree is untouched.
- **Check ordering:** `resolve_server` runs before `unload()` in `load()`, so a
  missing per-model exe does **not** kill the currently running server. This
  is intended and should be stated in the docs.
- **Load-failure UX:** the error is a status-bar label that clears after ~6 s
  (`note(e, true)`), and no log is written — the exe is resolved before
  `log_path`/`File::create`. The troubleshooting entry must not say "open
  Logs" for this error.

## Files to change

1. `src/model.rs` — `ModelConfig` gains `#[serde(default)] pub server: Option<String>`.
   No other changes; `server_args()` stays as-is. (`#[serde(default)]` is
   redundant for `Option` but consistent with the `args` field above it.)
2. `src/server.rs` — new `resolve_server(model: &Model) -> Result<PathBuf, String>`:
   - `Some(p)` → `PathBuf::from(p)`; if `!is_absolute()` →
     `Err("llama-server path must be absolute: <p>")`; if `!is_file()` →
     `Err("llama-server.exe not found: <p>")`.
   - `None` → `find_llama_server().ok_or("llama-server.exe not found")`.
   - `load()` calls `resolve_server(model)?` instead of `find_llama_server()?`.
3. `C:\aiModels\Inkling-2-Mini\Inkling-2-Mini.toml` — add
   `server = "C:/aiModels/_runtime/unsloth-b12588/llama-server.exe"`. Keep the
   comment explaining why; keep `args` untouched.
4. `INSTALL.md` —
   - §3 (lookup order): one sentence noting a per-model `server` key overrides
     the chain, with no fallback, pointing to Advanced configuration.
   - Advanced configuration: the new key — absolute path, forward slashes or
     single-quoted literal, must be named `llama-server.exe`, and the
     "model card can edit `alias` and `args`" sentence must include `server`
     so nobody hunts for a UI field.
   - Troubleshooting: the new error wording (`llama-server.exe not found: <path>`),
     the status-bar-label UX, the fact that no log is written for this failure,
     and the two other ways it happens — a relative `server` path (reject it,
     keep the "must be absolute" rule) or `QUANTDOCK_LLAMA` pointing at a
     missing file.
5. `README.md` —
   - The `QUANTDOCK_LLAMA` mention: note the per-model `server` key overrides
     the chain.
   - Layout convention: note the optional `server` key.
   - First-run sentence ("The TOML needs `name`, `gguf` and `alias`"): add
     "plus optional `server`".
6. `AGENTS.md` — the runtime-paths bullet (llama-server lookup) and the
   code-map line for `src/server.rs`.

## Tests

- `src/model.rs` (tests): a TOML with `server = "..."` deserializes to
  `cfg.server == Some("...")` — guards the serde wiring.
- `src/server.rs` (tests): `resolve_server` with a missing path → `Err`;
  with a relative path → `Err`; with an existing absolute file → `Ok` and the
  returned `PathBuf` equals the input verbatim. Needs a `model_with_server()`
  helper in the test module (the `tmp_model` helper is private to `model.rs`);
  no temp dir is needed for the `Err` cases, and the `Ok` case uses a temp
  file. The `None` branch is the existing `find_llama_server()` behavior and
  stays untested — Rust 2024 makes `env::set_var` unsafe and tests run in
  parallel, so env mutation is not an option.
- Optional one-line guard: add `server = "..."` to
  `set_key_keeps_comments_and_other_keys` to prove `set_key` (Save) preserves
  the new key.

## What I'm not doing

- No UI field for the server, no global key, no discovery-time validation.

## Review history

- Round 1 (Hy4 flash Free): code approach correct; needed docs (§3 + README),
  a deserialization test, a test helper, the forward-slashes note, and a
  troubleshooting half-sentence. Folded in.
- Round 2 (DeepSeek V4.1 Flash Free): needed the rollout step (clear
  `QUANTDOCK_LLAMA`, retire the `.cmd`, restart), the corrected escaping note,
  the absolute-path enforcement, the status-bar/no-log UX note, the
  `llama-server.exe` naming constraint, AGENTS.md in the file list, the
  empty-value semantics, the temp-file test approach, and the
  `set_key` preservation guard. Folded in.
