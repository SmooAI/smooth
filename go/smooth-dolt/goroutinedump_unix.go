//go:build !windows

package main

import (
	"os"
	"os/signal"
	"runtime/pprof"
	"syscall"
)

// installGoroutineDump wires SIGUSR1 → dump all goroutine stacks to
// dumpPath. Lets us debug a hung `serve` from outside the process even
// when stderr was redirected to /dev/null. Unix-only: SIGUSR1 does not
// exist on Windows (see goroutinedump_windows.go for the no-op stub).
func installGoroutineDump(dumpPath string) {
	dumpCh := make(chan os.Signal, 1)
	signal.Notify(dumpCh, syscall.SIGUSR1)
	go func() {
		for range dumpCh {
			f, err := os.Create(dumpPath)
			if err != nil {
				continue
			}
			_ = pprof.Lookup("goroutine").WriteTo(f, 2) // 2 = full frames
			_ = f.Close()
		}
	}()
}
