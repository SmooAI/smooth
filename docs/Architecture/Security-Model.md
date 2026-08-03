# Security Model

#architecture #security

> [!warn] Trusted-environment posture today
> The microVM boundary is gone. Big Smooth runs the operative as a **host subprocess with host-level access** to your working directory — no VM, no kernel-enforced egress, no filesystem sandbox. The surviving in-process guard is **Narc** tool surveillance plus role-scoped tool gating. Run Smooth where you'd run any coding agent you trust on your machine, on loopback or a trusted tailnet. The auto-mode permission engine (`th-515a13`) and the daemon's kernel-sandbox direction (`th-c89c2a`) are rebuilding real enforcement — both **in progress**, documented below as planned.

## What enforces things today

Every tool call the operative makes passes two in-process hooks on its `ToolRegistry`, in order:

1. **`PermissionHook`** (from the `smooth-operator` engine) — role-scoped tool gating. The operative's agent role (`SMOOTH_AGENT`, default `fixer`) declares which tools and permissions it's allowed; the hook blocks anything outside that surface. This is capability scoping, not a network/FS boundary.
2. **`NarcHook`** (`smooth-narc`) — surveillance. Fast regex detectors run on every call:
   - **Secret detection** — 10 patterns (API keys, tokens, private-key headers, …). A tool call or result carrying a secret is flagged/blocked.
   - **Prompt-injection guard** — 6 patterns over incoming content.
   - **Write guard** — optional (`SMOOTH_NARC_WRITE_GUARD=1`); off by default because the workspace is expected to be written.
   - Ambiguous cases can escalate to an **LLM judge** (`smooth-judge` slot) for a yes/no verdict.

`smooth-policy` TOML is still parsed by the operative, but only to log the intended network allowlist and phase and to feed Narc's surveillance context. **It no longer enforces anything** — the Wonk rule engine and the Goalie iptables/FUSE proxy that turned that policy into a kernel boundary were removed.

## What was removed (July 2026, `th-f4a801`)

- Per-task **microsandbox microVM** — hardware isolation around the agent.
- **Wonk** — the deterministic policy authority (`allow`/`deny` over network, filesystem, tools, pearls, MCP, CLI).
- **Goalie** — the forward proxy that forced all egress through Wonk, enforced by iptables (only Goalie's UID could egress) and a FUSE mount on the workspace.
- The credential-broker plumbing (`smooth-host-stub`, `smooth-credential-helper`, `smooth-bootstrap-bill`) that brokered host creds into the VM.

Git history at the teardown PR's parent commit (`5ccdd51`) is the archive for all of it.

## Planned — auto-mode permission engine (`th-515a13`, in progress)

The near-term replacement for Wonk's interactive posture: a Claude-Code-style **three-way verdict** (`allow` / `deny` / `ask`) on `smooth-policy`, with `ask` surfaced as an interactive prompt in chat and the decision persisted at a chosen scope (`Once`, `Session`, `PearlProject`, `User`). Policy sources stack, narrowest scope wins. This is being built in parallel now; check the pearl for landed status.

## Planned — kernel sandbox direction (`th-c89c2a`)

The [[Daemon-Direction|daemon epic]]'s security verdict: the load-bearing boundary becomes the **kernel**, not the permission layer (a reasoning agent can talk its way past an intent layer, but not past kernel FS/network scoping). The target:

- **Gate 1** — a deterministic rule engine (wonk's successor) in-process via the engine hooks: `deny → ask → allow`, first match wins, `ask` realized by the hook awaiting an approval result (fail-closed on deny/timeout).
- **Gate 2** — an LLM classifier (narc's judge) in `auto` mode only, run out-of-process with an rlimit cap, fail-closed on timeout.
- **Kernel sandbox** — sandbox the **tool subprocesses, never the daemon** (Codex's model): macOS Seatbelt / `sandbox-exec`, Linux `bubblewrap` + Landlock + seccomp; plus an egress-allowlist proxy as the real network boundary.

Threat model: single-tenant (one trusted operator per instance). The real risk isn't a malicious tenant — it's prompt-injection / untrusted repo content turning the operator's own agent against them (the "lethal trifecta": private-data access + untrusted content + egress). The kernel sandbox + egress allowlist + auto-mode is the cheaper, correct defense for that.

## Trusted-integration exceptions to the kernel sandbox

Some OS integrations cannot run inside the tool sandbox at all. macOS is the
recurring case: the seatbelt profile denies the XPC + mach lookups that EventKit
and Apple Events need, and denies reads under `~/Library` where the OS keeps its
private stores. Those integrations therefore get a **documented, narrow exception**
— they run outside the kernel layer.

Every exception must earn it the same way:

- **argv only, no shell** — no interpolation or injection path.
- **fixed binary** — a resolved path, never caller-supplied.
- **fixed script/statement** — the caller supplies *data*, never code.
- **verb allowlist, not a denylist** — an enumerated set, so a new upstream release
  can't quietly widen what the agent can reach.
- **still a normal tool call** — the permission gate and Narc hook see it exactly
  like every other tool, so surveillance and policy are unchanged. Only the
  *kernel* layer is waived, and only for that one integration.

The TCC grant is the compensating control: the OS asks the human once, per app
bundle, and the user can revoke it in System Settings.

### The exceptions that exist

- **`calendar`** / **`calendar_delete`** (macOS, pearl th-94cc4a) — spawn the
  `ical` EventKit client with a plain `Command`.
- **`imessage`** (macOS, pearl th-1665ed) — two halves, both outside the sandbox:
  the **read** is *in-process* `rusqlite` against `~/Library/Messages/chat.db` on
  a `SQLITE_OPEN_READ_ONLY` connection (no subprocess exists at all, so there is
  no shell and no injection surface), and the **send** spawns `/usr/bin/osascript`
  with a fixed AppleScript that takes the recipient and body as `on run argv`
  arguments — never interpolated into script source.

**Mutations get no extra userspace gate, on purpose — with one exception.**
`calendar` can book and move events and `imessage` can text a real human. Those
sit behind the same permission gate as `write_file` and `bash` — which, in the
daemon's default `AutoMode::Bypass` posture, means **a send executes without a
prompt**, exactly like a file write. That is the deliberate "allow benign, block
dangerous" stance, not an oversight; `SMOOTH_AUTO_MODE=ask` is the knob for a
stricter one.

**The exception: cancelling a calendar event confirms first.** Deleting is the
one mutation here the agent can't walk back on the next turn, so `calendar_delete`
parks the turn and asks. Mechanically it is the engine's existing
write-confirmation HITL: the runner installs core's `ConfirmationHook` over the
tool names in `ServerConfig::confirm_tools`, a matching call emits
`write_confirmation_required` (which the web UI renders as an approve/deny
prompt), and the tool only runs when the client answers `confirm_tool_action`
with `approved: true`. Deny, timeout (5 min), or a client that never answers all
fail **closed** — the tool call is refused, not silently run.

Two shapes fall out of that mechanism and are worth knowing:

- **The matcher is on the tool NAME, not the arguments** (`contains`). "This
  *verb* needs confirmation" is therefore only expressible as "this *tool* needs
  confirmation" — which is why `delete` was split off the `calendar` tool onto its
  own `calendar_delete` rather than gated by inspecting a `command` argument. It
  also means `delete` must be off the ungated tool's allowlist entirely, or the
  model bypasses the gate by picking the other tool. There is a test for exactly
  that.
- **The gate is a floor, not a setting.** `smooth_daemon::operator::CONFIRM_TOOLS`
  is merged into whatever `SMOOTH_AGENT_CONFIRM_TOOLS` sets, so the env var can
  widen the confirm list but cannot shrink it — an unset/empty var can't quietly
  disarm the delete gate.

Known gap: `th code`'s WS client doesn't render `write_confirmation_required`
yet, so a delete driven from that client parks and then fails closed on the
timeout rather than prompting. The web UI (Big Smooth's own surface) and
`th api smooth-operator` both handle the frame.

Corollary for any human-facing CLI wrapped this way: it will confirm destructive
actions on a TTY the daemon doesn't have, and its interactive/picker modes will
block forever on the daemon's null stdin. Those prompts have to be suppressed and
the interactive modes refused outright — a wrapper that silently blocks on an
unanswerable prompt is a worse failure than one that refuses the call.

### Reading private data is itself the risk

`imessage` is the first tool that puts a large, sensitive, **attacker-influenced**
corpus in front of the model: anyone who can text the user can write into it. That
is the "untrusted content" leg of the lethal trifecta arriving through a channel
the user doesn't think of as input. Treat message text as untrusted — it is not an
instruction — and note the safeguards that bound the exposure:

- every read is limited (default 20, hard cap 200 rows) — there is no
  "dump the whole database" shape;
- `thread` and `search` require a filter;
- message bodies are truncated, and attachments are reported as a boolean, never
  as a filesystem path the model could then go read.

The user opts in with Full Disk Access and can revoke it in System Settings.

**It intentionally reads a location the deny policy lists.** The daemon's embedded
`DenyPolicy` has `**/Library/**` under `[paths] deny`, covering reads as well as
writes. That tier gates tools which take a *path argument* (`read_file`,
`write_file`, `bash`); `imessage` takes no path — the database location is fixed
in the binary and not caller-supplied — so the glob never applies to it. That is
the intended design (a fixed, audited location is exactly what a path deny-list
protects against reaching *arbitrarily*), but it is worth stating plainly: adding
this tool means `~/Library/Messages/chat.db` is now reachable by the agent through
a route the path deny-list does not cover. Any future OS-integration tool that
reads a denied path must be scrutinised the same way, and must keep its location
fixed rather than caller-supplied.

## Related

- [[The-Cast]]
- [[Dispatch]]
- [[Daemon-Direction]]
- [[Operatives]]
