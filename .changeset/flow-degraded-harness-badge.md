---
'@smooai/smooth': patch
---

SmoothFlow pickers flag a harness that launches but won't fully work (th-51bf88). The daemon now runs the `th harness doctor` checks itself, in the background, and adds `health: {verdict, reason?, fix?}` to every row of `flow.hello` / `flow.harnesses`. When a verdict changes it broadcasts the list again. On macOS the New Session sheet labels such a harness "needs setup" and shows why: Codex's untrusted hooks, a stale smooth-agent plugin without the flow hook, a missing login. It also gives the one fix command to copy. The harness stays startable. The doctor's checks moved from `smooth-cli` into `smooth_flow::doctor` so the CLI and the daemon share one implementation. `th harness doctor` prints exactly what it did before. `SMOOTH_FLOW_HARNESS_DOCTOR=0` turns the daemon's pass off.
