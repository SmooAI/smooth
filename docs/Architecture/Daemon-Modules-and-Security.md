# Daemon Modules and Security

#architecture

> [!info] From CLAUDE.md
> smooth-daemon (Big Smooth) module map, in-process dispatch, and the three security layers. Moved verbatim out of `CLAUDE.md` §4.

## 4. Key Modules (smooth-daemon)

Big Smooth has **no bespoke server and no bespoke agent loop**. It hosts
smooth-operator's `LocalServer` (canonical WS protocol + widget) and adds its
own routes through the engine's `serve_routes` seam. Entry point:
`serve_local_flavor` in `operator.rs`.

| Module                         | Purpose                                                                                                                                       |
| ------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `lib.rs`                       | Crate root; `serve_local_flavor` re-export + `start_egress_proxy` (the goalie egress boundary)                                                |
| `operator.rs`                  | The local deployment flavor — builds and runs the operator `LocalServer` in-process, wires tool providers and hooks                           |
| `operator_storage.rs`          | Durable SQLite `StorageAdapter` so conversations/sessions survive restart (no Postgres)                                                       |
| `hooks/mod.rs`                 | The two engine `ToolHook`s installed on every per-turn registry: permission gate, then Narc                                                   |
| `hooks/narc.rs`                | `NarcHook` — regex detectors on tool args (secrets, prompt injection, dangerous shell), LLM-judge escalation, secret redaction in `post_call` |
| `config.rs`                    | Daemon config + LLM credential resolution (env → providers.json → gateway), egress config                                                     |
| `schedule.rs` / `scheduler.rs` | Proactive/scheduled turns; `SqliteScheduleStore` persists them, the tick loop fires them via a `TurnDriver`                                   |
| `search.rs`                    | `GET /search` — the `@`-mention autocomplete backend for the web composer                                                                     |
| `cwd_route.rs`                 | `GET`/`POST /api/session/cwd` — the UI's `/cd` and `/pwd`                                                                                     |
| `flow_route.rs`                | `/api/flow/*` — the SmoothFlow WS (`/api/flow/ws`), HTTP siblings, the Claude Code hooks long-poll, and the supervision tick (th-7f0af3)      |
| `relay.rs`                     | Smoo Relay bridge; routes `channel:"flow"` envelopes to the flow WS and caps phone-bound `flow.output` (16 KiB / ~30 fps)                     |
| `auth_login.rs`                | Browser OAuth2 + PKCE sign-in to Smoo AI, routed through the daemon (works over a tailnet origin)                                             |
| `push.rs`                      | Web Push — VAPID-signed notifications to the installed PWA                                                                                    |
| `tailscale.rs`                 | Best-effort `tailscale serve` exposure of the loopback listener                                                                               |

### Dispatch

There is no per-task worker process. A message arrives on the operator's
canonical WebSocket, the engine runs the turn in-process, and tools execute
against the host filesystem through `smooth-tools` — `bash` as the user (or
inside the opt-in kernel sandbox), egress pointed at the goalie proxy when one
is configured. Events stream back over the same
canonical WS to every client (`th code`, the web SPA, SDK clients).

> **microVM sandboxed dispatch removed 2026-07 (pearl th-f4a801).** Big Smooth
> used to spawn a per-task microsandbox microVM (mounting a cross-compiled
> `smooth-operative` at `/opt/smooth/bin`, bind-mounting the workspace) with a
> per-VM Wonk/Goalie/Narc/Scribe cast enforcing network + filesystem policy.
> The interim host-subprocess `smooth-operative` dispatch that replaced it is
> also gone. Git history and
> [ADR-004](../Decisions/ADR-004-remove-microvm-sandbox-stack.md) have the
> details.

### Security Architecture

Two layers always, a third opt-in, in the order a tool call meets them:

1. **Permission gate** — the engine's `permission::PermissionHook`, built in
   `smooth-daemon/src/operator::permission_hook`, layered with the daemon's
   embedded declarative `DenyPolicy` circuit-breakers. Installed **FIRST**, so a
   policy deny short-circuits before surveillance and before the tool runs.
   Modes/allow-lists live in `smooth-policy/src/auto_mode.rs`; see
   [`docs/Engineering/Auto-Mode-Permissions.md`](../Engineering/Auto-Mode-Permissions.md).
2. **Narc** (`smooth-daemon/src/hooks/narc.rs`) — surveillance. `pre_call` regex
   detectors (secret exfiltration, prompt injection, dangerous shell ops) with
   fail-closed LLM-judge escalation on ambiguous hits; `post_call` redacts
   detected secrets out of the tool result in place.
3. **Kernel OS sandbox** (`smooth-tools/src/sandbox.rs`) — **opt-in, OFF by
   default** (pearl th-efbab1). Turn it on with `th settings set
sandbox.enabled true` and restart Big Smooth (or `SMOOTH_SANDBOX=1` in the
   daemon's environment, which wins over the file). Big Smooth is a personal agent that operates AS its
   user on the user's own machine, and the sandbox got in the way of exactly
   that: asked to `ssh smoo-hub` and `git fetch`, ssh timed out (direct outbound
   kernel-denied behind the egress proxy) and git failed with "Operation not
   permitted" on `~/.ssh/known_hosts` (credential-store read-deny). So by
   default `bash` is a normal user subprocess — the user's env, `HOME`,
   `SSH_AUTH_SOCK` and `PATH`, minus only the daemon's own config (`SMOOTH_*`, `SMOOAI_GATEWAY_KEY`) — and
   layers 1 and 2 are the safety net. When on, `bash` subprocesses get
   **reads and writes** denied on credential stores (`~/.ssh`, `~/.aws`,
   `~/.config/gh`, `~/.kube`, `~/.docker`, `~/.gnupg`, `~/.netrc`, and the
   daemon's own `~/.smooth` secrets), write denies on `.git/hooks` /
   `.git/config` in every repo and `~/Library/LaunchAgents`, and the full
   secret-env scrub. With a proxy configured it is then also the **egress
   boundary**: direct outbound is kernel-denied except loopback, so traffic must
   pass goalie's exact-host allowlist. With the sandbox off, a configured
   allowlist still runs and still sets `HTTP(S)_PROXY`, but it is **advisory**.
   The daemon logs one startup line stating which posture it is in.
   `SandboxedCommand` is the only way `bash` builds a subprocess either way:
   pass-through is a mode of that type, not a second spawn path.

    ⚠️ **macOS only.** The enforced mode is Seatbelt-backed and exists nowhere
    else (th-08e05a). `SMOOTH_SANDBOX=1` on Linux or Windows logs a warning and
    runs `bash` unsandboxed. Before shipping a Windows build read
    [`docs/Architecture/Windows-Security-Posture.md`](../Architecture/Windows-Security-Posture.md),
    which enumerates exactly what is exposed there.

**Sidekicks** (`send_sidekick`) get the same first two layers. The engine
builds a sidekick's registry fresh, without host hooks, so the daemon wraps
each tool in the snapshot it hands the engine in the same hook chain, same
instances, same order (`smooth-daemon/src/hooks/sidekick.rs`, th-8d1951). A
sidekick's `Ask` reaches the user over the parent turn's approver; with no
approver it fails closed.

Removed with the microVM stack (2026-07, pearl th-f4a801; see git history):
**Wonk** (per-VM access authority), Goalie's per-VM FUSE + iptables enforcement,
and the "Big Smooth is READ-ONLY inside The Safehouse VM" isolation model.

---

## Related

- [[Home]]
