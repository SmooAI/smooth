---
'@smooai/smooth': patch
---

Pearl th-3c0b13: route every CLI "Smooth" / "Smoo AI" through the
gradient wordmark helpers.

The `crates/smooth-cli/src/gradient.rs` helpers (`smooth()`,
`smoo_ai()`) already existed but were only used in a handful of
places. Every other user-facing `println!` printed the brand name
as plain bold/colored text, so the same word read three different
ways depending on which command was running. This patch swaps the
literal "Smooth" / "Smoo AI" / "Big Smooth" / "Smoo AI Gateway" /
"Smoo AI platform" / "Smooth Operators" / "Smooth home" mentions
in `th`'s console output for calls to the existing gradient helpers
so the wordmark renders consistently with the logo (Smoo
orange→pink, th teal→blue).

Touches the bare-`th` explainer, `th up`, `th down`, `th status`,
`th auth status`, `th auth login` picker, `th operators`, `th
inbox`, `th doctor`, and `boot_ui.rs`'s `✻ Smooth booting` header.
Status / auth columns lose their `{:<N}` width formatting (which
would have been confused by the ANSI escapes) in favour of hand-
padded spacing so the visible columns still line up.

Tracing logs, error messages, identifier names, doc strings, and
the systemd unit file's `Description=` line are deliberately left
plain — those either land in log files or get piped/grep'd and
shouldn't carry ANSI.
