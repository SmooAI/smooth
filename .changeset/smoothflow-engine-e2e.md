---
'@smooai/smooth': patch
---

SmoothFlow engine e2e suite (th-8e3087): `crates/smooth-daemon/tests/flow_e2e` boots a real `smooth-daemon` per test — isolated HOME, ephemeral port, private tmux server, no lock / tailscale / relay / credentials — and drives it over the flow WS, the HTTP siblings, the `th` binary and `POST /api/flow/hooks`, with `fake-agent` installed through harness manifests in four state-source flavours (hooks, learned id, native, scrape). 24 tests (~70 s): shell lifecycle, every agent transition, the permission long-poll, resume-on-death with `--resume`, the three-resume give-up, the duplicate-resume guard, the hooks contract per event, `th flow` / `th harness` JSON, the harness matrix + sort/hide prefs, `th harness add`, and proof the suite never touches the real `~/.smooth`. Runs on every PR (tmux installed, `SMOOTH_E2E_STRICT=1`); docs in `docs/Engineering/SmoothFlow-Testing.md`.

Engine bugs the suite found and this release fixes: tmux under a non-UTF-8 locale (any daemon not started from a shell) rewrote the tab in the pane-dead / pane-size queries, so a dead pane was never detected and supervision was inert; `{daemon_url}` was rendered from a port-0 request address; a resumed or prompt-less harness stayed `starting` forever (`SessionStart` now makes a starting row idle); a `held` row was flapped by the supervisor.
