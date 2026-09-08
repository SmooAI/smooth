#!/bin/bash
# Print what TCC thinks of THIS process: who macOS holds responsible for it,
# whether it can read a Full-Disk-Access-protected file, and its Calendar
# status. Run it from a terminal, from a tmux pane the app started, from a
# pane the daemon started — the differences are the attribution matrix in
# docs/Architecture/SmoothFlow-macOS.md.
label="${1:-probe}"
# `launchctl procinfo` needs root; libquarantine's responsibility_get_pid_responsible_for_pid does not.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tool="${TMPDIR:-/tmp}/smoothflow-responsible"
[ -x "$tool" ] || clang -o "$tool" "$here/responsible.c" 2>/dev/null
resp="$("$tool" $$ 2>/dev/null)"
rname=""; [ -n "$resp" ] && rname="$(ps -o comm= -p "$resp" 2>/dev/null | xargs basename 2>/dev/null)"
# stat() is allowed on protected paths; only open() is gated, so actually read.
if head -c1 "$HOME/Library/Application Support/com.apple.TCC/TCC.db" >/dev/null 2>&1 || ls "$HOME/Library/Mail" >/dev/null 2>&1 && [ -d "$HOME/Library/Mail" ]; then fda=granted; else fda=denied; fi
daemon="$(command -v smooth-daemon || echo "$HOME/.cargo/bin/smooth-daemon")"
cal="$("$daemon" tcc calendar 2>/dev/null | tail -1)"
printf '%s\tpid=%s\tppid=%s\tresponsible=%s(%s)\tfda=%s\tcalendar=%s\n' "$label" "$$" "$PPID" "${resp:-?}" "${rname:-?}" "$fda" "${cal:-?}"
