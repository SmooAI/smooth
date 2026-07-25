---
'@smooai/smooth': patch
---

smooth-dolt now builds on Windows (pearl th-5f35a5, groundwork for a
Windows-safe pearl server).

The only thing stopping the embedded-Dolt binary from compiling on Windows
was one line — `syscall.SIGUSR1`, a Unix-only signal used for the
goroutine-dump diagnostic hook. The whole CGO stack (gozstd, and with the
`gms_pure_go` tag no ICU is needed) compiles and runs on Windows fine; it
was never the ICU/CGO yak the old CI comment assumed. The SIGUSR1 hook is
split into `goroutinedump_unix.go` (real impl) / `goroutinedump_windows.go`
(no-op), and CI now builds `smooth-dolt.exe` on `windows-latest` so it
can't silently regress with another unix-only call.

This does not yet make the pearl *server* run on Windows — that still needs
the Unix-socket transport swapped for TCP loopback (same pearl, next step).
Verified on a throwaway Windows EC2 driven over SSM;
docs/Operations/Windows-Build-Box-Runbook.md documents the loop.
