# SmoothFlow keybindings, tabs and splits

> Pearl th-27baa4. Code: `apps/smoothflow/Sources/Keys/` (the keymap),
> `Sources/UI/PaneTree.swift` + `PaneTreeView.swift` + `SurfaceTabBar.swift`
> (the surface model), `Sources/UI/KeyboardPane.swift` (Settings ▸ Keyboard).
> Shell overview: [SmoothFlow-macOS.md](../Architecture/SmoothFlow-macOS.md).

SmoothFlow is a terminal you steer a fleet of agents from, so it owes you the
two things every terminal has and it did not: a **surface model** (tabs holding
splits, split in a direction you chose) and a **keymap you own**. Both landed
together, because a keymap with nothing to bind is a settings pane, and a split
tree you cannot reach from the keyboard is a demo.

## The surface model

A **tab** holds a **binary split tree**. A **pane** is a leaf of that tree, and
shows one session's terminal surface — or nothing yet.

```
tab 1                      tab 2
┌─────────┬─────────┐      ┌───────────────────┐
│         │  pane   │      │                   │
│  pane   ├─────────┤      │       pane        │
│         │  pane   │      │                   │
└─────────┴─────────┘      └───────────────────┘
```

- **Tabs hold panes, not sessions.** A session lives in the sidebar and belongs
  to the fleet; a tab is a _layout over_ the fleet. Two tabs can show the same
  session, and closing a tab never touches a session. That is the opposite of a
  browser tab, and it is the right model here: the sessions outlive the window
  (they are tmux sessions the engine owns), so a tab that owned one would be
  lying.
- **⌘W closes the pane, and the container collapses when it empties** — last
  pane closes the tab, last tab closes the window. That is what Ghostty, iTerm2
  and Terminal.app do. ⌘⇧W still closes the whole tab, splits and all. See
  [Closing a pane](#closing-a-pane) for the confirmation, which is the
  interesting half.
- **A split shows what it was split from.** ⌘D on a pane running `claude` gives
  you two panes on that same session, which is what "split this" means in every
  terminal. Point one of them somewhere else from the sidebar.
- **Panes are identified, not indexed** (`PaneID`, monotonic, never reused).
  Removing a pane therefore cannot silently retarget another pane's surface,
  and a `TerminalSurfaceView` survives a split, a close, a zoom and a tab switch
  — the scrollback lives in the surface, so losing one loses history.
- **Zoom does not change the layout.** `SurfaceTab.zoomed` names a pane; the
  tree underneath is untouched, so unzoom is exact.
- **Directional focus is geometry, not tree walking.** `paneInDirection` takes
  the leaf frames and picks the nearest edge in that direction, breaking ties on
  the other axis' centerline — the rule every tiling WM uses, and the only one
  that behaves when the layout is not a neat grid. `PaneNode.frames(in:)`
  computes those frames from the same fractions the `NSSplitView`s lay out with,
  so the model and the screen cannot disagree.

Only fractions travel back from the view: a dragged divider writes
`settingFraction` into the model without a rebuild, or the drag would fight the
relayout. The hierarchy is rebuilt only when the tree's _skeleton_ changes.

## Closing a pane

Because a pane is a view over a session the engine owns, ⌘W has **two honest
answers** when a live session is on screen, and the alert offers both rather
than guessing:

| Button                                            |                                                                                  |
| ------------------------------------------------- | -------------------------------------------------------------------------------- |
| **Close Pane** / **Close Tab** / **Close Window** | the view goes; the session keeps running and stays in the sidebar                |
| **End Session** (destructive)                     | kills the process too                                                            |
| **Cancel**                                        | **the default button** — a stray Return over this sheet must never kill an agent |

`PaneClose.decide` is a pure function of the pane's session and returns the
prompt, or `nil` for "just close it". It asks only when there is something to
lose:

| Pane holds                            | ⌘W                                                |
| ------------------------------------- | ------------------------------------------------- |
| nothing                               | closes, no dialog                                 |
| a `done` / `dead` session             | closes, no dialog — the pane is scrollback        |
| a **shell at a prompt** (`idle`)      | closes, no dialog                                 |
| a **shell with a foreground process** | asks, naming it                                   |
| any **harness** session, live         | asks, naming the harness, the pearl and the state |
| anything, with the setting off        | closes, and never kills                           |

The duplicate-view line matters more than it looks: ⌘T starts the new tab on
the focused session and a split starts on the session it was split from, so
without it the most ordinary ⌘T-then-⌘W and ⌘D-then-⌘W would both raise a scary
dialog about a session that is still sitting right there in the tab you came
from. Closing one of several views of a session destroys nothing.

The idle-shell line is Ghostty's own `confirm-close-surface` nuance: an alert
people learn to dismiss unread is worse than no alert. The agent line is the
reason the dialog exists at all — the copy names the harness ("Claude Code is
still working on th-27baa4"), says the pane can be closed without killing it,
and says plainly that ending the session _"kills the process, and an agent
killed mid-turn loses the work in flight."_

> **Why not libghostty's own confirmation?** `GhosttyRuntime.baseOverrides` sets
> `confirm-close-surface = false` and keeps it that way. libghostty does not
> know these surfaces are views over engine-owned sessions, so its generic
> "close this surface?" would be both wrong and unskippable — no idle-shell
> nuance, no harness name, no close-without-killing option. The flag stays off
> and SmoothFlow asks its own question.

"Don't ask again" is on the sheet because it maps to a real setting —
Settings ▸ Terminal ▸ _Confirm before ⌘W closes a pane holding a live session_
(`PaneCloseSettings`, `terminal.confirmClosePane`). With it off, ⌘W closes the
view immediately and never ends a session, which is the safe direction for a
switch people flip while annoyed.

## The keymap

`FlowAction` (`Sources/Keys/FlowAction.swift`) is the single source of truth:
every keyboard-reachable action, its menu title, its category, and the chord it
ships with. The menu bar is **built** from it — one selector
(`AppDelegate.runAction`) with the action on `representedObject` — so a rebind
moves the menu with it and there is exactly one code path that can be wrong.
Adding an action is one enum case plus one line in `AppDelegate.run`.

### The file

`~/.smooth/smoothflow/keybindings.toml`, a `[keys]` table of
`action = "chord"`:

```toml
[keys]
splitRight = "opt+cmd+d"   # rebind
steerAll   = "shift+cmd+enter"
inbox      = ""            # unbind
```

- Modifiers: `cmd` (`command`/`super`/`meta`), `shift`, `opt` (`option`/`alt`),
  `ctrl` (`control`). Keys: one character, or `enter` `tab` `space` `escape`
  `backspace` `delete` `left` `right` `up` `down` `home` `end` `pageup`
  `pagedown` `f1`–`f20`. Case and order do not matter; `cmd++` is ⌘+.
- **Only what you changed belongs in the file.** An action you never touched
  keeps the app's default, so new defaults in a later release still reach you.
  Re-typing a default is not an override and is dropped.
- **A bad line loses that line, never the map.** Unknown actions, unparseable
  chords and bare keys (no modifier — they would swallow terminal input) become
  entries in `Keymap.problems`, shown at the top of Settings ▸ Keyboard. An
  unreadable file falls all the way back to defaults: shortcuts are how you
  reach a fleet of agents, and a stray bracket must not take them all away.
- **Conflicts are reported, never resolved.** Two actions on one chord is
  flagged in the pane (and AppKit's own answer — first matching menu item wins —
  is at least stable). Guessing which one you meant would be worse.

### The pane

Settings ▸ Keyboard (⌘,) lists every action by category with its current chord,
a recorder, a per-row reset, and Reset all. It writes the same file, and
"Reveal file" opens it — neither surface can be the stale one. "Reload" re-reads
the file for the person who edited it by hand while the app ran.

The recorder is an `NSView`, not a SwiftUI button, and it overrides
**`performKeyEquivalent`** as well as `keyDown`: ⌘-anything goes to the menu bar
first and never reaches `keyDown`, so a recorder that only implements `keyDown`
can never capture a chord that is already bound — which is most of them. ⎋
cancels, ⌫ unbinds.

One AppKit trap is pinned by a test: **shift belongs in
`keyEquivalentModifierMask`, never in the character.** An uppercase key
equivalent already implies shift, so an item that sets both asks for ⇧⇧ and
simply never fires.

## Two default changes

Both are rebindable, and both are deliberate.

**Steer All Working moved ⌘⇧↩ → ⌘⌥↩.** ⌘⇧↩ is zoom-the-pane in Ghostty, iTerm,
tmux-with-a-prefix and cmux; a terminal fleet console that disagrees with every
terminal is the one that is wrong. ⌘⌥ is already the fleet-action family here
(⌘⌥Y allow, ⌘⌥N deny, ⌘⌥R kill & resume, ⌘⌥K kill), and steering every working
session is exactly a fleet action. `KeymapTests` pins both halves so nobody
walks it back by accident.

**⌘D is Split Right and ⌘⇧D is Split Down**, replacing the old untyped "Split
Surface" that could only ever add another column. ⌘⇧← and ⌘⇧↑ are the other two
directions; ⌘⇧→ and ⌘⇧↓ are deliberately left free for anyone who wants the
symmetric four-arrow set instead of ⌘D/⌘⇧D — that is two rows in the pane.

**⌘W is Close Pane, ⌘⇧W is Close Tab.** ⌘W acts on the surface in every
terminal, so it does here; ⌘⇧W keeps its place as the bigger hammer. There is no
`closeSplit` action any more — ⌘W is it.

## Tests

- `Tests/PaneCloseTests.swift` — the ⌘W decision: what is asked and what is
  not, the wording for each session state and each scope, the name fallbacks,
  and the setting's default.
- `Tests/KeymapTests.swift` — chord parsing (aliases, `+` as both separator and
  key, garbage), wire round-trip, menu key equivalents, the default map's
  freedom from conflicts, file parsing with bad lines, override/unbind
  semantics, serialization.
- `Tests/PaneTreeTests.swift` — splitting in four directions, collapse on
  remove, fraction targeting and clamping, frame geometry (including "first is
  the upper pane", which the arrow keys depend on), directional focus with no
  wrap-around, and the `SurfaceTab` operations.
- `UITests/LayoutUITests.swift` — the real app against the mock fleet: ⌘D/⌘⇧D
  split, ⌘W asks before closing a live agent's pane (Cancel leaves it alone),
  ⌘⇧W takes the whole tab, ⌘⇧↩ zooms, ⌘T/⌘W and ⌘⇧[ / ⌘⇧] move between tabs, splits
  stay in their tab, and the Keyboard pane renders.

## Deliberately not in this pass

- **Layout persistence across relaunch.** Tabs and splits are per-run.
- **A command palette** (⌘⇧P over sessions, actions and harnesses).
- **Chord sequences** (`ctrl+a` then `|`, tmux-style). The file format has room
  for it — the value is a string — but nothing parses a second chord yet.
- **Per-context bindings.** A chord means the same thing everywhere; there is no
  "only when the terminal has focus" scope.
