---
'@smooai/smooth': minor
---

SEP Phase 3 (smooth) — `th ext` + the extension host substrate.

**`th ext`** manages SEP extensions: `install ./path [--project] [--trust]` copies a local extension directory into `~/.smooth/extensions` (or `<repo>/.smooth/extensions`), prints its declared capabilities, and prompts to trust it; `list` shows installed extensions with their trust state; `trust <name>` records trust; `remove <name>` deletes it. Trust is **content-hashed and fail-safe** — an extension only loads when it's recorded trusted in `~/.smooth/extensions/trust.toml` and its `extension.toml` still hashes to the value trust was granted against (editing re-locks it), and a non-interactive install never trusts silently.

**`smooth_code::sep_host`** is the frontend host substrate: a `RenderBlock` model for `set_widget` payloads (markdown/keyvalue/progress + text fallback), the `UiSink` trait that decouples `ui/request` from the ratatui event loop, `TuiUiProvider` (the engine `HostDelegate` that routes `select`/`confirm`/`input`/`notify`/`set_status`/`set_widget`/`set_title` onto a `UiSink`), the trust store, and `load_trusted_host` (discover → trust-gate → `ExtensionHost::load` declaring the seven TUI ui capabilities).

**Engine pin** flipped from crates.io `0.14.0` to a git rev of `smooai-smooth-operator-core` `main` (SEP Phases 0–3, incl. the `ui_capabilities` handshake), which is not yet in a crates.io release. Flip back to a version pin once a release publishes the extension module.

The live agent runs in `smooth-operative`; relaying a dispatched operative's `ui/request` to the TUI is SEP Phase 6 (the daemon event surface). This ships the CLI, trust model, and tested render/host substrate it builds on.
