---
'@smooai/smooth': patch
---

`th ci-queue` gates on how busy the CPUs actually are, not on load average.

macOS load counts threads blocked in the kernel as well as runnable ones. On 2026-09-30 the load read ~108 on 12 cores with only 4 runnable threads, most of it `exec` waiting on `syspolicyd` to check freshly built test binaries. The 4/core load gate, and the AIMD scale it drove to its 0.25 floor, held 20 heavy jobs for 15–24 minutes behind one on a mostly idle machine.

- New gate signal `max_cpu_busy_pct` (default 90): CPU busy % from the kernel's tick counters, smoothed across every `th` into a ~10 s average (`~/.smooth/ci-queue/cpu.json`). It holds heavy jobs and drives the AIMD scale together with memory, swap and disk.
- Load average is now a far backstop: `max_load_per_core` defaults to 12, and it only counts while the CPUs are at least 50% busy (or unreadable). The starvation nights it guards against (load 180–270, 1,022) were CPU-bound too.
- `status`, `top` and the web view show the CPU reading.
