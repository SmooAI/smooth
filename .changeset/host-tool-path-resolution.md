---
'@smooai/smooth': patch
---

Fix `host_tool` spawn under macOS launchd-managed Big Smooth: the
inherited PATH was minimal and didn't include `/sbin` (ping, route)
or Homebrew dirs, so `host_tool({tool: "ping", ...})` failed with
`spawn failed: No such file or directory`. Now resolves the tool's
absolute path against a richer search list (`/usr/local/bin`,
`/opt/homebrew/bin`, `/usr/bin`, `/bin`, `/sbin`, `/usr/sbin`)
before spawning; falls back to letting Command walk inherited PATH
when nothing matches.
