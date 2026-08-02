---
'@smooai/smooth': patch
---

Big Smooth can read and adjust your macOS Calendar (pearl th-94cc4a, first slice).

- **`calendar` tool** (macOS only) — a first-class tool over the
  [`ical`](https://github.com/BRO3886/ical) EventKit CLI. Reads: `today`,
  `upcoming`, `list`, `search`, `show`, `calendars`, `free`, `inbox`. Writes:
  `add`, `update`, `delete`. Always JSON. It is the first **documented exception**
  to "every subprocess goes through the kernel sandbox": seatbelt blocks
  EventKit's XPC/mach lookups, so it spawns `ical` with a plain `Command` — argv
  only, fixed binary, verb allowlist, and still a normal tool call the permission
  gate and Narc hook see.
- **`ical`'s human-first modes are neutralized**, because the daemon spawns it
  with null stdin: `-i` is refused, `update`/`delete` require an event id (a bare
  one opens a picker), and `delete` always gets `--force`. The decision point is
  the tool's permission gate, not a TTY prompt nothing can answer.
- **Platform-specific tool registry** — the daemon cfg-gates the tool to macOS
  (Linux/Windows never see it) and registers it even when it can't work yet: a
  missing `ical` or an ungranted TCC permission answers "run
  `th doctor --setup-calendar`" instead of failing opaquely.
- **`th doctor --setup-calendar`** — side-loads the `ical` release binary to
  `~/.smooth/bin/ical` (no Homebrew tap), then drives Big Smooth.app into asking
  macOS for Calendar access and reports what's left to click.
- **Native EventKit grant request** — `smooth_menubar::eventkit` (the macOS
  quarantine crate) calls `EKEventStore.requestFullAccessToEvents` at app
  startup, which is what makes the OS prompt appear; a bare CLI asking gets a
  silent denial.
- `Info.plist` also declares `NSRemindersFullAccessUsageDescription` now, so the
  reminders slice doesn't need a re-sign.
