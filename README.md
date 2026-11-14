# QuantDock

A lightweight desktop manager for local GGUF models (llama.cpp / `llama-server`) on Windows.

## What it's for

Running several local LLMs means juggling launchers, aliases and ports by hand. QuantDock puts
that in one window:

- **Load / unload** models from a single list
- **Per-model config files** — tune context, sampling, KV cache, speculative decoding by editing
  one `.toml`, without touching a launcher
- **One OpenCode entry** that always tracks whichever model is currently running
- **Floating resource monitor** — always-on-top, transparent overlay for GPU / VRAM

## Stack

Rust + [egui](https://github.com/emilk/egui). No Node, no bundled webview — one small native
binary that uses the OS GPU stack directly.

## Status

Early scaffolding. The model layout, per-model `.toml` config scheme and the shared `local`
alias scheme are already designed and working; see the `old/` notes and the working setup in
`C:\aiModels` on the dev machine.

## Layout convention

Each model lives in its own folder alongside its config:

```
<Model Name>/
  <model>.gguf
  <Model Name>.toml
```
