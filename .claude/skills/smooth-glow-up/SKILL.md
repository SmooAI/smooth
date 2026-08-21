---
name: smooth-glow-up
description: Glow up a Smooth surface — `th code`'s TUI, `th` CLI output, or the smooth-web SPA — in the Presence design language. Smooth is a personal agent you cohabit with, so color is spent as PRESENCE and ATTENTION (the teal→blue `th` face, amber only when he needs you), never as decoration. Sibling to smooai-glow-up (Aurora) but for a terminal-first product. Use when building or reshaping ANY Smooth UI. Triggered by `/smooth-glow-up` or phrases like "glow this up", "make th code look good", "polish the TUI", "make it feel like Smooth".
---

# Smooth glow-up — the Presence design language

Glow up the target surface with **Presence**, Smooth's design language. Like its
sibling `smooai-glow-up`, this is a thin wrapper around real design craft — it
supplies the _identity_ the craft must honor.

**Presence is not Aurora.** Aurora (smooai) is a dashboard language: a cool
midnight ground and a teal→gold→coral spectrum spent as _heat_ — pipeline stage,
priority, temperature. Smooth is a **personal agent you cohabit with**, mostly
met in a terminal. Its ground is **warm**, and its color is spent on **presence
and attention**, not measurement. Don't import Aurora's heat ramp here; don't
export Presence's warmth there.

## How to run it

1. **Understand the surface first.** Read the code that renders it and, where you
   can, _look at it running_. Never restyle from imagination.
2. **Honor what already ships** — the implementation is the source of truth:
    - **TUI**: `crates/smooth-code/src/theme.rs` — every color and semantic style
      function already lives here (`th_gradient`, `smoo_gradient`, `panel_border`,
      `tool_status_border`, `input_border`, `file_color`, …). Extend this module;
      never hardcode a `Color::Rgb` at a call site.
    - **Web SPA**: `crates/smooth-web/web/src/globals.css` — the `@theme` block is
      the token set (`--color-background`, `--color-panel`, `--color-coral`,
      `--color-amber`, `--color-online`, `--color-th-teal`, `--color-th-blue`).
      Add tokens there, not inline.
    - **CLI**: the `th` command surface — plain, pipe-safe output (see below).
3. **Verify by rendering, not by reasoning** (see _Verification_). A TUI diff you
   haven't seen drawn is a guess.
4. **Follow the repo workflow** — worktree per pearl, colocated `#[cfg(test)]`
   tests, `cargo fmt` + `cargo clippy` + `cargo test` green, changeset, PR. See
   CLAUDE.md §8–§10.

---

## The Presence design language

> The one idea everything hangs on: **the teal→blue `th` gradient is Big Smooth's
> face.** It marks where _he_ is present — his turns, his mark, his heartbeat —
> and nothing else. Chrome never wears the face. Everything else stays quiet so
> presence reads instantly.

### Ground: warm, not corporate

Smooth's neutrals are **warm** near-black and warm off-white — deliberate, so the
product feels like a room you share, not a dashboard you audit.

| Role     | Web token                            | TUI (`theme.rs`)               |
| -------- | ------------------------------------ | ------------------------------ |
| ground   | `--color-background` warm near-black | terminal default bg (inherit!) |
| raised   | `--color-panel` / `--color-panel-2`  | border, not fill               |
| text     | `--color-foreground` warm off-white  | `SMOO_WHITE`                   |
| muted    | `--color-muted-foreground`           | `MUTED`                        |
| hairline | `--color-border`                     | `panel_border(false)`          |

**Never** cool grey/blue-black in Smooth (that's Aurora's ground), and never pure
grey — the warmth is the point.

### The face (reserved)

- `--color-th-teal` `#00a6a6` → `--color-th-blue` `#1238dd`; in the TUI,
  `th_gradient()` / `th_gradient_color()`.
- **Reserved for Big Smooth's presence**: his wordmark, his turns, his
  alive/heartbeat indicator. Never for buttons, borders, or section headers.
- The `smoo` orange→red gradient (`smoo_gradient()`) is the _brand wordmark_ only.

### Attention (spend sparingly, mean it)

- **coral** `--color-coral` — primary / "yes" / the affirmative action.
- **amber** `--color-amber` — **only** "Big Smooth needs you" (an approval gate, a
  blocked run). If amber appears for anything else it stops meaning anything.
- **online** `--color-online` — awake/alive status.
- semantic: `success()` / `error()` in `theme.rs`; keep them distinct from the face.

### Terminal translation (what replaces glass and blur)

A TUI has no blur, shadow, or gradient fill. It has **fg/bg, bold/dim/italic/
reverse, box-drawing, and whitespace**. Translate the intent, don't fake the effect:

| Web idea        | Terminal equivalent                                                                                                           |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| glass/elevation | a **border** (`panel_border(active)`), or nothing — never a bg fill                                                           |
| gradient fill   | a **per-character gradient** across a short run of text only (`gradient_row`, `th_gradient_color`) — never across a paragraph |
| drop shadow     | one blank line of breathing room                                                                                              |
| hover/active    | `Modifier::BOLD` + brighter border; reserve `REVERSED` for selection                                                          |
| disabled        | `MUTED` fg, never a grey bg                                                                                                   |

**Background fills are the TUI's cardinal sin**: they fight the user's terminal
theme, break transparency, and smear in copy/paste. Style the **foreground** and
let their terminal be the ground.

### Restraint rules

- **Spend boldness once per screen.** One focal element (the wordmark, the active
  panel, the streaming turn) — everything else recedes.
- **Encode state in FORM, not just color**: a status glyph (`●`/`○`/`◐`), a border
  weight, an indent. ~8% of users can't read your hue distinction, and
  `NO_COLOR`/piped output has no color at all.
- **Alignment beats ornament.** Column-align counts, timings, and money
  (right-align numerics) before adding any color.
- **Motion**: a spinner is enough. No animated ASCII beyond a single quiet
  indicator; never animate on every event.

### CLI output (the `th` surface)

- **Pipe-safe by default**: respect `NO_COLOR`, and drop styling when stdout isn't
  a TTY. A `th … | grep` must never contain escape codes.
- Machine-readable escape hatch (`--json`) stays _unstyled and stable_.
- Errors: one line saying what failed, one line saying what to do next. Never a
  wall of Rust backtrace at a user.
- Progress on stderr, results on stdout — so redirection works.

### Don't

Background fills · the `th` face on chrome · amber for anything but "needs you" ·
cool/grey neutrals · a gradient longer than a word or two · color as the _only_
carrier of meaning · emoji as status glyphs (they wreck column alignment in half
the terminals out there).

---

## Verification — you must SEE it

The web skill says "screenshot the rendered component." The terminal equivalent:

- **Snapshot test the render** — ratatui's `TestBackend` lets you draw into a
  fixed-size buffer and assert on it. This is the runnable check to leave behind
  for any non-trivial layout logic.
- **Look at it live** — run the TUI under tmux and capture the pane:
    ```bash
    tmux new-session -d -s glow -x 120 -y 40 'th code'
    sleep 3 && tmux capture-pane -p -t glow     # add -e to keep escape codes
    tmux kill-session -t glow
    ```
- **Check the degraded paths too**: `NO_COLOR=1`, a narrow width (80 cols), and a
  non-TTY pipe. A design that only works at 200 columns in truecolor isn't done.

## Porting to another terminal product

Keep the **method** — face color reserved for presence, attention colors that
mean exactly one thing, foreground-only styling, form-not-just-color, pipe
safety — and swap the hexes for that product's identity. The method travels; the
Smooth hexes don't.
