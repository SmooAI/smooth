---
'@smooai/smooth': patch
---

SMOODEV-1787 (PR 1/4, dual-engine collapse): consume the published
`smooai-smooth-operator-core` engine instead of the in-tree copy, and
delete the in-tree `crates/smooth-operator/`.

The in-tree engine and the public `smooth-operator-core` were the same
engine but had diverged. The only differences were (a) the public core
gates its BigSmooth control-plane reporter behind a `bigsmooth` cargo
feature (with a no-op stub when disabled) and (b) cosmetic
public-sanitization edits (doc rewording, neutralized example hosts in
tests, `smooth_operator` → `smooth_operator_core` in doc examples, a
provider-agnostic `ModelRouting::default()`, a redacting `Debug` for
`ProviderConfig`/`LlmConfig`, and a wider retry-status set). smooth never
enables the `bigsmooth` feature and never sets a reporter, so the gated
reporter calls were dormant no-ops — the cutover loses nothing.

Wiring: the workspace dep KEY stays `smooth-operator` and is package-aliased
to `smooai-smooth-operator-core`, so all ~12 consumers' `use smooth_operator::…`
imports compile unchanged. Pinned as a rev-locked git dep (not a sibling
path dep) to avoid the CI `cargo metadata` failure that SMOODEV-1464 hit
with a `../`-style path dep. No functional change, no module removal — that
lands in later PRs.
