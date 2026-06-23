# Always-on daemon — security model

The reborn Big Smooth (`smooth-daemon`, EPIC th-c89c2a) drops the per-task
microVM in favour of a **single-tenant, always-on daemon** whose security rests
on three independent layers. The threat model is **not** untrusted tenants (we
don't have those) — it's **prompt-injection turning the operator's own trusted
agent against them**: the "lethal trifecta" of private-data access + untrusted
content + network egress → exfiltration. Each layer below removes one leg of
that trifecta, and the load-bearing two are **kernel-enforced** (an agent can
reason its way around a userspace check, but not around the kernel).

| Layer | Enforces | Kernel? | Source |
|---|---|---|---|
| **Gate 1 — permission engine** | *intent*: what runs freely / asks / is denied | no (UX layer) | `crates/smooth-daemon/src/permission.rs`, `hook.rs` |
| **Kernel sandbox** | filesystem confinement + credential/env secrecy | **yes** (macOS Seatbelt) | `crates/smooth-tools/src/sandbox.rs` |
| **Egress boundary** | network: exact-host allowlist, no direct off-box | **yes** (Seatbelt net-deny + proxy) | `crates/smooth-goalie/` |

All three are **opt-in by environment variable** so the loopback-default dev
experience is unchanged; turn them on for an exposed / always-on deployment.

---

## Layer 1 — Gate-1 permission engine (intent)

A deterministic `deny → ask → allow` decision for every tool call, modelled on
Claude Code's permission modes. It expresses *intent and UX*; it is **not** the
security boundary (the kernel layers are). Decisions are pure functions of
`(mode, tool_name, args)`, so they're exhaustively testable.

**Modes** (`SMOOTH_PERMISSION_MODE`, runtime-switchable via `PUT /api/mode`):
`default` · `acceptEdits` · `plan` · `auto` · `dontAsk` · `bypassPermissions`.

- Read-only tools (`read_file`, `list_files`, `grep`) are always allowed.
- Writes consult protected-path rules; mutating `bash` consults a read-only
  classifier; the posture per mode decides allow/ask/deny.
- **Circuit-breakers fire in *every* mode, including `bypass`** — `rm -rf /|~`,
  fork bombs, disk-destroying `dd`/`mkfs`, and remote-code-execution
  (`curl … | sh`/`python`/`perl`/…, `eval "$(curl …)"`). These are the last
  backstop before the kernel sandbox.

An `ask` decision is realised by the [`PermissionHook`](../../crates/smooth-daemon/src/hook.rs)
blocking on the durable approval queue: approve → `Ok`, deny/timeout →
**fail-closed** `Err`.

## Layer 2 — kernel sandbox (filesystem + secrets)

`bash` is the only tool that spawns a subprocess, and it does so **only** through
[`SandboxedCommand`](../../crates/smooth-tools/src/sandbox.rs) — there is no
constructor that yields an unsandboxed `Command` (P0: "run without sandbox" is
architecturally impossible). On macOS the shell runs under a generated Seatbelt
profile that:

- confines **writes** to the workspace (+ temp); additionally **denies writes**
  to `.git/hooks` and `.git/config` (either could re-enter execution outside the
  sandbox via a hook or `core.hooksPath`);
- **denies reads** of credential stores — `~/.ssh`, `~/.aws`, `~/.config/gh`,
  `~/.config/gcloud`, `~/.kube`, `~/.docker`, `~/.gnupg`, `~/.netrc`, **and the
  daemon's own `~/.smooth/providers.json` (LLM key) + `~/.smooth/auth` (JWT)** —
  so a tool can't exfil what drives the agent;
- **scrubs secret-named environment variables** (`SMOOTH_*`, `*_API_KEY`,
  `*_TOKEN`, `*_SECRET`, `*PASSWORD*`, `*_PAT`, …) from the child env, so a
  read-only-classified `env`/`printenv` leaks nothing.

> **Platform status:** macOS Seatbelt is **enforced**. Linux (bubblewrap +
> Landlock + seccomp) is **TODO** — the shell currently falls back to an
> unsandboxed subprocess with a loud warning, acceptable only for the
> single-trusted-user loopback daemon.

## Layer 3 — egress boundary (network)

The trifecta's exfil leg. When `SMOOTH_EGRESS_ALLOWLIST` is set (comma/space
separated **exact hosts** — the `defaults` token expands to a curated set of
package registries + source hosts + the Smoo platform, and merges with any of
your own hosts), the daemon:

1. builds an [`EgressAllowlist`](../../crates/smooth-goalie/src/allowlist.rs)
   through a single strict hostname parser — rejecting, *before* the membership
   check, the bypass primitives that defeat host allowlists: embedded NUL /
   non-ASCII labels (the `attacker.com\0.google.com` SOCKS5 class,
   CVE-2025-55284), ports/schemes/paths, and malformed labels. **Exact hosts
   only** — wildcard/port entries are dropped (and logged), so a bad config can
   only *narrow* reachability, never widen it;
2. starts goalie's in-process forward proxy (`run_proxy_local`) on a loopback
   port (`SMOOTH_EGRESS_PROXY_ADDR`, default `127.0.0.1:4419`) that decides each
   request against the allowlist (fail-closed) and audits it;
3. routes the `bash` tool's egress through it — `HTTP(S)_PROXY` point at the
   proxy **and the Seatbelt profile denies direct `network-outbound` except to
   loopback**, so a tool that ignores the proxy vars simply can't connect
   off-box. The proxy (running outside the sandbox) is the only path out.

## Auth + bind hardening

For a daemon reachable over a tailnet:

- `SMOOTH_DAEMON_TOKEN` enables a bearer-token gate on every API + WS route
  (`/health` and the SPA stay open); the token may be presented as
  `Authorization: Bearer <token>` or `?token=` (browser WebSockets), compared in
  constant time.
- Default bind is loopback (`SMOOTH_DAEMON_BIND`); a non-loopback bind with no
  token logs a startup warning.

---

## Configuration summary

| Env var | Effect | Default |
|---|---|---|
| `SMOOTH_PERMISSION_MODE` | Gate-1 posture | `default` |
| `SMOOTH_DAEMON_TOKEN` | bearer-token auth (opt-in) | unset (open on loopback) |
| `SMOOTH_EGRESS_ALLOWLIST` | exact-host egress allowlist (opt-in); `defaults` expands the curated set | unset (egress unrestricted) |
| `SMOOTH_EGRESS_PROXY_ADDR` | egress proxy bind | `127.0.0.1:4419` |
| `SMOOTH_DAEMON_BIND` | daemon bind | `127.0.0.1:4400` |

## Related

- [[Architecture-Overview]]
- [[Sandboxed-Mode]] · [[Direct-Mode]] — the prior microVM model this replaces
- Permission semantics mirror Claude Code's auto-mode permission model.
