---
'@smooai/smooth': patch
---

`th ci-queue`: `cargo nextest run --archive-file` (the shim and `run`) no longer takes the machine-wide cargo lock, so test execution overlaps the next job's compile. iOS jobs (`attest ios`, `xcodebuild`) take an `ios` lock (`[run] serialize_ios`, default on) and get a realistic estimate floor (`[budget] ios_rss_gb`, `ios_cores`). A `--max-wait` timeout now prints `NEVER RAN: … waited 120m` (exit code still 75). Shim marker bumped to v3: re-run `th ci-queue shim install`.

A stall watchdog kills a running job whose process tree (descendants by parent pid, across groups and sessions) averaged under 5% of a core for 20 minutes (`[run] stall_minutes`, exit 76, outcome `stalled`) along with its busy re-parented helpers. CPU belonging to a job flagged idle no longer holds the gate or collapses the adaptive scale, light jobs are no longer held by the heavy memory/CPU budget, `th ci-queue status` shows the top CPU consumers with their working directory and owning job, and `[run] max_runtime_heavy_secs` / `max_runtime_light_secs` add an optional runtime cap. A job flagged idle releases its memory/CPU reservation, `attest rust` gets a default ~2h runtime cap (`[run] max_runtime_by_label_secs`, killed as `stalled`), and `th attest ios` now takes the `ios` lock too.
