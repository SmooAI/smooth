---
'@smooai/smooth': minor
---

SEP Phase 5 (smooth) — packaging, skills unification, marketplace search, legacy deletion.

**`th ext install` gains npm/git sources.** Beyond a local directory, install from `npm:@scope/pkg[@version]` or `git:host/user/repo[@ref]`. npm packages are vendored under `~/.smooth/extensions/.npm` (an `npm install --prefix` tree so their deps resolve), git repos under `~/.smooth/extensions/.git/<host>/<path>` at the pinned ref (and `npm install`ed when they carry a `package.json`). A `~/.smooth/extensions/<name>` symlink to the vendored dir is what the engine discovers, so packaged and local installs load identically through the engine's existing top-level discovery. A manifest may be `extension.toml` **or** a `smooth` key in `package.json` (synthesized into `extension.toml` at install). Trust records the source spec so `th ext update [<name>]` re-fetches and reconciles — an unchanged manifest keeps its trust, a changed one is re-locked (fail-safe).

**`th ext search <query>`** matches a curated index shipped in the binary plus live npm packages tagged with the `smooth-extension` keyword, printing the install command for each hit.

**Skills unification.** A trusted extension's `[resources] skills` directory now feeds the one canonical skill catalog (`smooth-cast`) via `skills::resources_discover`; each SKILL becomes a `/skill:<name>` with source `extension`, gated on the same content-hashed trust (an untrusted extension contributes no skills — the skill body is a prompt-injection surface). The duplicate skill parser in `smooth-code` (`extensions.rs`) is deleted; `/skill` and the new `/ext` TUI command both read from `smooth-cast`. `SkillSource` gains an `Extension` variant and a shared `label()` (replacing five inline `match` copies).

**`/ext` TUI command** lists installed extensions with their trust state and declared capabilities. Live command/UI dispatch into a running host reaches the TUI over the daemon event surface (SEP Phase 6).

**Deletions (migration verdicts).** The zero-consumer `smooth-plugin` trait crate is removed (in-process trait plugins are exactly what SEP rejects), and the duplicate `smooth-code` skill parser is removed in favor of `smooth-cast`.
