---
'@smooai/smooth': patch
---

Big Smooth can read your macOS Calendar (pearl th-94cc4a, first slice).

- **`calendar` tool** (macOS only) — a first-class read-only tool over the
  [`ical`](https://github.com/BRO3886/ical) EventKit CLI: `today`, `upcoming`,
  `list`, `search`, `show`, `calendars`, `free`, `inbox`, always in JSON. It is
  the first **documented exception** to "every subprocess goes through the kernel
  sandbox": seatbelt blocks EventKit's XPC/mach lookups, so it spawns `ical` with
  a plain `Command` — argv only, fixed binary, read-verb allowlist, and still a
  normal tool call the permission gate and Narc hook see.
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
