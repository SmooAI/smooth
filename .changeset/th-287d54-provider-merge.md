---
'@smooai/smooth': minor
---

SEP Phase 7 (smooth) — `th cast models` surfaces extension-registered providers.

`th cast models` now folds in LLM providers contributed by globally installed
extensions (`~/.smooth/extensions/`). Any extension that registers a provider via
the SEP `registerProvider` surface is loaded headlessly and its declared models
are listed under an `extension <ext>.<provider>` section (in `--json`, as
`<provider>/<model>` ids). Model ids are filtered + sorted like gateway/local
models; a provider left with no matching models is dropped. Loading is gated to
**global** extensions — a plain `th cast models` in a repo never spawns a project
extension — and any failure yields an empty list, so extension providers are
strictly additive and can't break the core listing.

**Engine pin** bumped to the `smooai-smooth-operator-core` git rev carrying SEP
Phase 7 (registerProvider / OAuth / proxied streaming / `session/set_model`),
which exposes `ExtensionHost::providers()`.
