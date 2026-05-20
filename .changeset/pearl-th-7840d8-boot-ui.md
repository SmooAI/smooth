---
'@smooai/smooth': patch
---

Pearl th-7840d8: animated boot UX for `th` cold start + `th up`.

Replaces the bare `Starting Smooth...` line (and the silent gap
during `th up`'s daemon spawn) with a per-step indicatif spinner
cascade so the user can see what's happening while the Safehouse
microVM and the in-VM cast services come up.

Steps shown in both entry points:

```text
✻ Smooth booting
    ✓ starting Safehouse microVM
    ✓ cast online (wonk · goalie · narc · scribe · archivist · diver · groove)
    ✓ operator-runner pool warm
    ✓ health check
```

Spinners turn into a green `✓` on success or a red `✗ — <reason>`
on timeout / failure. The boot transcript stays in the terminal
after `th up` returns. v1 drives the steps off observable TCP +
HTTP probes against `localhost:4400`; no daemon-side IPC needed.

New module `crates/smooth-cli/src/boot_ui.rs` with a tested
`BootIndicator` / `BootStep` state machine. Adds `indicatif` to
the workspace deps.
