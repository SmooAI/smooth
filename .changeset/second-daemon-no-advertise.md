---
'@smooai/smooth': patch
---

smooth-daemon: a second instance (`SMOOTH_ALLOW_SECOND_DAEMON=1`, e.g. the SmoothFlow app's child daemon) no longer overwrites `~/.smooth/daemon.addr`. Only the primary daemon advertises, so launching SmoothFlow no longer repoints `th`, the hooks and Big Smooth's clients at the wrong daemon (th-3e6b1b).
