---
'@smooai/smooth': patch
---

Add the `smooth-glow-up` skill — Smooth's **Presence** design language.

Sibling to smooai's `smooai-glow-up` (Aurora), but for a terminal-first
product. Aurora is a dashboard language: cool midnight ground, teal→gold→coral
spent as *heat*. Presence is for a personal agent you cohabit with: a **warm**
ground, and color spent on **presence and attention** — the teal→blue `th`
gradient reserved for Big Smooth himself, amber meaning only "he needs you".

Grounded in what already ships (`crates/smooth-code/src/theme.rs`,
`crates/smooth-web/web/src/globals.css`) and covers all three Smooth surfaces:
the `th code` TUI, `th` CLI output, and the smooth-web SPA — including the
terminal translation of web ideas (borders not glass, foreground-only styling,
form-not-just-color), pipe/NO_COLOR safety, and how to actually *see* a TUI
change (ratatui `TestBackend` snapshots + tmux `capture-pane`).
