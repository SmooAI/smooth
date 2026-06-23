---
'smooai-smooth-cli': patch
---

Add `th daemon audit` — tail the egress proxy's JSON-lines audit log
(`~/.smooth/audit/egress-proxy.jsonl`) and print the recent allowed/blocked
off-box network decisions as compact `ts ALLOW/BLOCK METHOD host` rows
(`--lines N`, default 20; friendly message when no log exists yet). With
`th daemon status` showing whether the egress boundary is on, this lets the
operator see what it actually did. Pure `format_audit_line` is unit-tested;
verified live against a running daemon (a blocked + an allowed decision).
