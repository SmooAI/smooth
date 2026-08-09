#!/usr/bin/env python3
"""Tool-usage metrics from a convo run's transcripts.jsonl.

    python3 scripts/the-line/tool-stats.py ~/.smooth/bench-runs/convo-XXXX/transcripts.jsonl

`smooth-bench convo` prints this block itself now. This exists for the
runs that came before it, and for re-reading an archived run without
paying to re-run it — the transcripts already carry every turn's tool
calls with their ok/error outcome, so the arithmetic is the same.

The columns, and why each one is here:

  rate     pass rate over conclusive trials — the headline, and the one
           that hides everything below
  calls    total tool calls
  err%     calls whose result came back an error
  /turn    calls per turn. Two models can tie on rate while one takes
           twice the calls to get there; that is the difference between
           an agent you can leave running and one you cannot
  judge    the judge's 1-5 tool_use axis, averaged
  silent   turns that made NO tool call. High on a suite whose tasks all
           need tools means the model is answering from memory
"""
import json, sys, collections

path = sys.argv[1]
by = collections.defaultdict(lambda: {
    "calls": 0, "errors": 0, "turns": 0, "silent": 0,
    "judge": [], "pass": 0, "conclusive": 0,
})
for line in open(path):
    d = json.loads(line)
    m = by[d["model"]]
    st = d["status"]
    if st != "INCONCLUSIVE":
        m["conclusive"] += 1
        if st in ("PASS", "XPASS"):
            m["pass"] += 1
    if d.get("scores"):
        m["judge"].append(d["scores"]["tool_use"])
    for t in d.get("turns", []):
        m["turns"] += 1
        calls = t.get("tools") or []
        if not calls:
            m["silent"] += 1
        m["calls"] += len(calls)
        m["errors"] += sum(1 for c in calls if c.endswith("error"))

print(f"{'model':24} {'rate':>6} {'calls':>6} {'err%':>5} {'/turn':>6} {'judge':>6} {'silent':>7}")
rows = sorted(by.items(), key=lambda kv: -(kv[1]['pass'] / max(kv[1]['conclusive'], 1)))
for name, m in rows:
    rate = 100 * m["pass"] / max(m["conclusive"], 1)
    err = 100 * m["errors"] / max(m["calls"], 1)
    per = m["calls"] / max(m["turns"], 1)
    judge = sum(m["judge"]) / len(m["judge"]) if m["judge"] else None
    j = f"{judge:.1f}" if judge is not None else "-"
    print(f"{name:24} {rate:5.1f}% {m['calls']:6} {err:4.0f}% {per:6.1f} {j:>6} {m['silent']:4}/{m['turns']}")
