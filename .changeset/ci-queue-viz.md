---
'@smooai/smooth': minor
---

The machine-wide check queue can now be watched live (SMOODEV-3371).

`th ci-queue web --open` serves a live page from `th` itself, with no daemon needed, and streams the queue to it over Server-Sent Events:

- Jobs flow from the FIFO line through a gate into the heavy and light slot lanes, as a three.js stream of work. While the pressure gate holds heavy jobs the stream piles up at the gate, and a locked lane runs gold.
- Every waiter shows what the queue says holds it.
- Running jobs show elapsed time against their usual run.
- The page glows teal → gold → coral with the machine's pressure.
- Panels show pressure gauges (every threshold on one gate line, plus ten-minute sparklines), the schema-2 admission budget (each job's estimated vs actual slice, and the AIMD scale's sawtooth), lock holders, per-check cost profiles, and recent jobs.

`--demo` (or `?demo`) replays the 2026-09-26 night (35 agents, load climbing toward 1,000, memory exhausted, the gate holding, then the drain), so the page can be shown on an idle machine.

Big Smooth gets a Queue tab with the same view, fed by a new `GET /api/ci-queue/status`, and the menu bar gets a Check Queue item.

`th ci-queue top` draws the same picture in a terminal: it works over ssh, falls back from truecolor to 256 colours or no colour, and drops sections as the window shrinks.
