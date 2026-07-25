//go:build windows

package main

// installGoroutineDump is a no-op on Windows: the SIGUSR1 goroutine-dump
// hook is Unix-only (Windows has no SIGUSR1). The unix build carries the
// real implementation in goroutinedump_unix.go.
func installGoroutineDump(_ string) {}
