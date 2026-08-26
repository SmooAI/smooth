---
'@smooai/smooth': patch
---

bench: the rust engine (our main flavor) can now boot. `score --engine rust`
launched `th daemon`, whose single-instance guard refuses to start whenever any
Big Smooth is already running (they'd fight over `~/.smooth/operator-storage.db`)
— which is always true on a dev box or smoo-hub, so the rust engine scored 0/18
every run, blowing the 300s boot timeout. It now boots isolated:
`SMOOTH_ALLOW_SECOND_DAEMON=1`, a per-task scratch `HOME` (its own `~/.smooth`),
and gateway creds mirrored onto `SMOOTH_API_URL/KEY/MODEL` (the daemon's tier-1
env, so an empty scratch HOME needs no providers.json). Verified: boots in ~0.5s
with coding tools wired and passes tasks (th-e493b4).
