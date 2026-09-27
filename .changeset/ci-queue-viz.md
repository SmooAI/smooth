---
'@smooai/smooth': minor
---

The machine-wide check queue can now be watched live (SMOODEV-3371).

`th ci-queue web --open` always gets you the page. If Big Smooth is running and has the Queue tab, it opens that. Otherwise, or with `--serve`, `th` serves the page itself and streams the queue to it over Server-Sent Events, from one sampler that only reads the queue while someone is watching. The particle layer is capped (360 particles, 30 fps, pauses while hidden), falls back to Canvas 2D without a real GPU, and is off under reduced motion. What the page shows:

- Jobs flow from the FIFO line through a gate into the heavy and light slot lanes, as a three.js stream of work. While the pressure gate holds heavy jobs the stream piles up at the gate, and a locked lane runs gold.
- Every waiter shows what the queue says holds it.
- Running jobs show elapsed time against their usual run.
- The page glows teal → gold → coral with the machine's pressure.
- Panels show pressure gauges (every threshold on one gate line, plus ten-minute sparklines), the schema-2 admission budget (memory committed against the pool, CPU against its budget, each job's memory now against its estimate, and the AIMD scale's sawtooth), lock holders, per-check cost profiles (peak memory across the process group against the largest single process), and recent jobs.

`--demo` (or `?demo`) replays the 2026-09-26 night (35 agents, load climbing toward 1,000, memory exhausted, the gate holding, then the drain), so the page can be shown on an idle machine.

Big Smooth gets a Queue tab with the same view, fed by a new `GET /api/ci-queue/status`, and the menu bar gets a Check Queue item.

`th ci-queue top` draws the same picture in a terminal: it works over ssh, falls back from truecolor to 256 colours or no colour, and drops sections as the window shrinks.
