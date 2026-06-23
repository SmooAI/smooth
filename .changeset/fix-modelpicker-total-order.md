---
"@smooai/smooth": patch
---

fix: model-picker sort comparator is now a total order (unblocks releases)

`candidate_models_filtered` sorted models by slot benchmark with
`y.partial_cmp(&x).unwrap_or(Ordering::Equal)`. A NaN benchmark makes
`partial_cmp` return `None`, and collapsing that to `Equal` violates total
order — which Rust's sort (1.96+) detects and **panics** on ("comparison
function does not correctly implement a total order"). That panic failed the
Release workflow's `cargo test` step on CI (whose model data hit the NaN path),
so every Release run failed, the "New version release" changeset PR never
auto-merged, and the version sat at 0.14.1 while changesets piled up. Switched
to `f32::total_cmp`, a proper total order that sorts NaN deterministically.
Pearl th-03b02e.
