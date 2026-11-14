# QuantDock

A lightweight desktop manager for local GGUF models (llama.cpp / `llama-server`) on Windows.

## What it does

- **Load / unload** models from one list, no juggling of launcher scripts
- **Per-model config files** — tune context, sampling, KV cache and speculative decoding by
  editing one `.toml`, without touching a launcher
- **Shows what is *actually* running** — the OpenAI-compatible server accepts any model id,
  so the label you picked is not proof. QuantDock reads `/props` and resolves the real GGUF.
- **Floating resource monitor** — a transparent, always-on-top overlay for GPU / VRAM / temp
- **Three OpenCode entries** — models are grouped by the `alias` in their `.toml`, so
  `local`, `local-3d` and `local-twil` map onto one entry each

## Stack

Rust + [egui](https://github.com/emilk/egui) (`eframe`, OpenGL via `glow`). No Node, no
bundled webview — two small native binaries that use the OS GPU stack directly.

## Build

```sh
cargo build --release
```

Produces `target/release/quantdock.exe` (the manager) and `target/release/monitor.exe`
(the overlay). Both are GUI-subsystem binaries — no console window.

Requires the MSVC toolchain (`rustup` default) and the Visual Studio C++ build tools
for the linker.

## Layout

```
<Model Name>/
  <model>.gguf
  <Model Name>.toml     <- per-model config (edit this, not a launcher)
```

The model root defaults to `C:\aiModels`; override with the `QUANTDOCK_MODELS`
environment variable. `llama-server.exe` is found automatically in the LM Studio
backends directory (highest version wins); override with `QUANTDOCK_LLAMA`.

### Config format

```toml
name = "Qwen3.8-27B-R3-mix"
gguf = "Qwen3.8-27B-R3-mix.gguf"   # relative to this folder
alias = "local"                     # shared by a group -> one OpenCode entry
args = "-dev CUDA0 -ngl 99 -c 262144 ..."   # llama-server flags, one line
```

QuantDock launches the model as:

```
llama-server.exe -m <gguf> -a <alias> <args> --port 8080
```

Saving a field from the UI rewrites only that key and **preserves comments and
formatting** in the rest of the file.

## Aliases and OpenCode

The `alias` in a model's `.toml` is the model id the server advertises, and the id
OpenCode must use. Models sharing an alias are interchangeable from OpenCode's point
of view — pick the entry that matches the group you started.

| alias | OpenCode entry | models |
|---|---|---|
| `local` | Local (Qwen3.8-27B, 262K) | R3-mix, Q4_K_M, TurboFCFusion, MSVC9 Matcher |
| `local-3d` | Local (3D Coder, 262K) | Qwen3.5-4B-3D-Coder |
| `local-twil` | Local (TwIL-8B, 131K) | TwIL-8B-verbose |

## Using it

- **Load** starts the model (it stops any running `llama-server` first).
- **Unload** stops the server.
- The **Monitor** button launches the floating overlay.
- In the overlay: drag to move, right-click to quit.

## Tests

```sh
cargo test
```

Covers config round-tripping (comments preserved), the generated `llama-server`
command line, and model discovery.
