# ADR-008: Family AI — multi-principal Big Smooth with per-member RBAC and Smoo Jr

- **Status**: Proposed
- **Date**: 2026-08-09
- **Pearls**: th-c5b97c (epic); builds on th-c89c2a (daemon), ADR-007 (relay)

## Context

Big Smooth (smooth-daemon) is **single-tenant by construction**. The persona is
literally "Big Smooth, *the user's own* always-on assistant"; the filesystem and
shell tools are confined to *the owner's* `~/dev`; `recall`/`remember` is *the
owner's* memory; the bundled MCP tokens (Outlook, calendar) are *the owner's*
credentials. The security stack assumes one principal:

- `smooth-policy`'s permission gate (`PermissionRules`) is a **single global**
  deny/ask/allow matcher list with a fail-safe `Ask` default. There is **no
  principal/identity** anywhere in the permission hook or the operator wiring.
- The relay (ADR-007) forwards `{to, frame}` **same-user-only by construction**:
  the destination channel is keyed on the sender's own authenticated `sub`, so
  only one Smoo user's devices can reach a given daemon.
- `operator_storage` (SQLite) partitions by conversation/session, not by person.

We want a **Family AI**: several people share one Big Smooth, each with a
role-scoped view of its tools and data, and a **Smoo Jr** child mode with
fail-closed guardrails strong enough to hand a kid a phone. That is a genuine
single-tenant → multi-principal shift, and it is safety-critical: if a child
persona can reach a parent's Outlook, the guardrail is theater.

The load-bearing question is **identity**. A persona a client merely *asserts*
(a picker, a stored profile) is worthless as a guardrail — a jailbroken kid
session, or the app itself, simply claims to be "Dad". The guardrail is only
real if the acting identity is **authenticated cryptographically** and every
gate keys off that verified identity.

## Decision

**Family = a Smoo org. Each member is a real Smoo user with a role. Each member
connects with THEIR OWN Supabase JWT, and the daemon applies that member's
role-scoped RBAC + Narc profile to every tool call.** Identity is never
self-asserted; it is the verified `sub` of the connecting member.

```
                    family org (Smoo)
   parent  (owner)  ─┐
   teen    (member) ─┤ each dials the family daemon with THEIR OWN jwt
   child   (smoo-jr)─┘
                     ▼
              relay.smoo.ai  ── org-scoped routing (new)
                     ▼
              smooth-daemon  ── resolves member → role → PermissionRules + Narc profile
                     ▼
        permission gate (per-principal) → Narc (per-role) → kernel sandbox
```

Four concrete changes, smallest-blast-radius first:

1. **A `Principal` threaded into the permission gate.** `PermissionRules`
   becomes *per-principal*: the daemon holds a role → rules map and selects by
   the verified identity of the frame's origin, not a global set. The matcher
   language and fail-safe `Ask` default are unchanged — only the *selection* is
   new. Deny-by-default for any principal without an explicit ruleset.

2. **Org-scoped relay routing.** Today a daemon is reachable only by its owner's
   `sub`. A **family daemon** accepts frames from any member of its org, each
   authenticated by their own JWT (verified org membership, not a shared token),
   and stamps the frame with the verified member id the daemon then keys RBAC
   on. Same-user-only becomes same-org-only, still structural, still tested
   adversarially.

3. **Per-principal memory & session partition** in `operator_storage`: Smoo Jr's
   conversations and `remember`/`recall` **must not** bleed into a parent's, or
   vice versa. Isolation is a storage partition keyed on the verified principal,
   not a prompt instruction.

4. **Smoo Jr — the hardened child role**, defense-in-depth, every layer
   fail-closed:
   - **Tools**: allowlist only. No `bash`, no `write_file`/`edit_file`, no
     arbitrary `web_search`/`crawl`, no `th`. Deny-by-default via (1).
   - **Egress**: goalie **exact-host allowlist** to kid-safe domains only — the
     boundary already exists (`SMOOTH_EGRESS_ALLOWLIST`); Smoo Jr just gets a
     tiny list and the kernel denies the rest.
   - **Model**: a safety-tuned model, pinned for the role.
   - **Narc**: child-safety detectors (self-harm, grooming, PII solicitation,
     age-inappropriate content) with fail-closed LLM-judge escalation.
   - **Parent oversight**: a parent **audit + approval inbox** — Narc already
     logs per-actor; surface it, and route Jr's `ask`-tier actions to a parent
     for a verdict.
   - **Limits**: time-of-day / rate caps per role.

## Security posture shift

Big Smooth stops being a machine with one owner and becomes a machine with one
owner *and delegated principals*. What holds the line:

1. **Authenticated identity, not asserted.** Every principal is the verified
   `sub` of a member's own JWT, checked against org membership. No persona
   picker, no shared family token — those are explicitly rejected below.
2. **Deny-by-default per principal.** The gate's fail-safe `Ask`/deny default
   now applies *per identity*: a principal with no ruleset can do nothing. A new
   family member is powerless until a parent grants a role.
3. **The existing three layers stay, now keyed on identity.** Permission gate →
   Narc → kernel sandbox all run per call as today; the change is *which* rules
   and *which* Narc profile, selected by verified principal. The kernel sandbox
   (workspace-confined writes, credential-store read denials, egress boundary)
   is unchanged and remains the load-bearing layer.
4. **Blast radius containment is the whole point.** A compromised or jailbroken
   Smoo Jr session can reach only Jr's allowlisted tools and kid-safe egress —
   never the parent's Outlook, files, shell, or memory. That containment is
   enforced at the gate + sandbox + storage partition, not by the model's
   goodwill.
5. **Legal — COPPA is a gate, not a footnote.** Real under-13 accounts trigger
   verifiable parental consent, data minimization, and a no-behavioral-ad-
   profiling obligation. Storing a child's conversations makes this binding.
   **No real under-13 account ships until the COPPA path is designed and
   reviewed.** (Building/dogfooding with the family's own kids under the parent
   account is fine; a public under-13 signup is not.)

## Alternatives rejected

- **Local personas on the parent's daemon (PIN/passkey-gated switch).** Ships
  fastest and needs no relay change, but the identity is self-asserted: the
  guardrail is only as strong as the switch, and a jailbreak or a modified
  client bypasses it. Acceptable as a *demo milestone that already plumbs
  per-principal RBAC keyed on an injected identity* (so the org model is a
  swap-in), **not** as the shipping guardrail for real children. This is the
  "phase B → A" path; A is the destination.
- **A shared family token / one account for the whole household.** Destroys
  per-member RBAC and audit — everyone is the owner. Rejected outright.
- **A separate daemon per family member.** Preserves single-tenant simplicity
  but multiplies always-on machines and can't share one family calendar/context;
  heavyweight for a consumer feature.
- **Enforcing kid guardrails purely in the system prompt / persona text.**
  Prompt-level rules are not a security boundary. Guardrails live in the gate,
  Narc, the egress allowlist, and the storage partition — code, not prose.

## Consequences

- The permission gate gains a principal dimension; every daemon call site that
  builds `PermissionRules` must select by identity. One-time refactor, then
  additive.
- The relay grows org-scoped routing — the meatiest new piece — and its
  adversarial test surface expands (cross-org denial, membership spoofing,
  role-escalation attempts).
- Memory/session storage grows a principal partition; migrations must not leak
  the existing owner's data into a shared space.
- Smoo Jr is only as safe as its allowlists and Narc detectors; those need
  explicit, exhaustively-tested coverage (adversarial child-safety inputs) and
  an owner-visible audit trail — treat it like the policy-enforcement code it is.
- COPPA/legal work is a hard prerequisite for a public child surface and should
  start in parallel, not after the engineering.
- Direct connection and the single-owner daemon remain first-class; a family is
  an opt-in org binding, not a new default.

## Implementation — M1 + M2 (pearl th-12d875)

Recon (a fan-out over the daemon seams) refined the injection point from the
original sketch. Two engine facts drove the final shape, both verified against
`smooai-smooth-operator-1.23.2`:

- The engine **drops `Principal.role`** at the connection boundary
  (`access_context()`), but **preserves `Principal.groups`**. So the role must
  ride as a `role:<name>` **group tag**, not the role field.
- `ToolHook` sees only `&ToolCall` (no principal), and the two security hooks are
  process-global singletons. So the permission *hook* cannot branch on caller —
  but the per-turn **`ToolProvider` can**, because it receives `ctx.access`
  (which carries `groups`) on every turn.

**Result: enforcement is tool-set narrowing in `SandboxedToolProvider::tools_for`,
with ZERO engine changes.**

- `crates/smooth-policy/src/family.rs` — pure policy: `FamilyConfig`
  (`members` + `roles`), `from_toml` (fail-closed), `member_for_token`
  (constant-time), `tool_allowed(role, tool)` = `PermissionRules.decide(tool,"")
  != Deny` (deny-by-default; unknown role → deny-all). Rules match **engine tool
  names** (lowercase `bash`, `write_file`, `read_file`, …), not Claude-Code labels.
- `crates/smooth-daemon/src/org_auth.rs` — `SmooOrgVerifier` gains an optional
  `FamilyConfig`. Owner secret → `Admin` (unchanged, checked first). Else a family
  member token → a `Basic` principal with `groups = ["role:<role>"]`, sharing the
  owner's org. Unknown token → `InvalidToken` (fail-closed).
- `crates/smooth-daemon/src/operator.rs` — after all tools are pushed and
  **before** the sidekick snapshot, `tools_for` reads the `role:` group and
  `retain`s only `tool_allowed` tools; `send_sidekick` (delegation) is gated the
  same way, closing the "delegate to a full-clearance runner" escape. Owner (no
  `role:` group) is never filtered. Loaded from `~/.smooth/family.toml`
  (`SMOOTH_FAMILY_FILE` override); absent/malformed ⇒ single-tenant, fail-closed.

**Smoo Jr = the `child` role** in `~/.smooth/family.toml`:

```toml
[[members]]
token = "<the Jr device's own local bearer token>"
id = "kid-alex"
role = "child"
display_name = "Alex"

[roles.child]
allow = ["read_file", "list_files", "grep", "recall", "get_current_datetime"]
default = "deny"   # everything else — bash, writes, web, th, MCP, delegation — is dropped
```

Jr's **no-egress** guarantee comes from denying every egressing *tool* at this
filter (goalie's proxy allowlist is process-wide, so a per-principal proxy isn't
possible) — with `default = "deny"` there is no code path from a Jr turn to the
network. **Smoo Jr also surfaces as a selectable mode** (`modes.ts`, id
`smoo-jr`) — UX + model pin only; the guardrail is the principal-derived tool
narrowing, independent of the mode.

## Implementation — M2.5 within-tool resource RBAC (pearl th-716243)

M1/M2 RBAC is **whole-tool**: a role either has the `calendar` tool or it
doesn't. M2.5 adds the next grain down — **which calendars** a role that *does*
have the tool may see and touch — the first *within-tool resource* allowlist.

- **Config:** `RoleProfile.calendars: Option<Vec<String>>`, parsed from
  `[roles.<name>] calendars = ["Family", "Kids Sports"]` in `family.toml`.
  Semantics match `tool_allowed`, **fail-closed**: absent ⇒ unrestricted (adult
  default), `Some([...])` ⇒ only those names, `Some([])` ⇒ none; an unknown role ⇒
  `Some([])`. Accessor: `FamilyConfig::calendars_allowed(role)`.
- **Binding:** the allowlist is injected onto the `CalendarTool` /
  `CalendarDeleteTool` instances in `tools_for` **at construction**, from the
  authenticated `role:` group — never a tool argument. Same un-settable-by-caller
  property as the M2 tool narrowing: a child cannot widen it.
- **Enforcement** (`smooth_tools::calendar`, the `ical` seam): reads
  (`today`/`list`/`upcoming`/`search`) with no caller-named calendar are **bound**
  to the allowed set with injected `-c` flags; any caller `-c/--calendar <name>`
  must be in the set; `--calendar-id`/`--exclude-calendar` are refused (can't be
  checked by name); `add` must target an allowed calendar (the default calendar
  may be outside the set); the `calendars` listing is post-filtered to allowed
  names, and calendar *management* (create/rename/delete) is refused. An empty
  allowlist hard-denies every event-touching verb.
- **Ceiling** (documented, not a hole): `ical delete`/`show`/`update`-in-place
  carry no verifiable calendar target, so gating there leans on the read boundary
  — a scoped member can only have learned event ids from allowed calendars.
  Tighten by resolving each event's calendar via a `show` first if it ever bites.

**Scope landed:** M1 (per-principal RBAC) + M2 (Smoo Jr role + mode) + M2.5
(per-role calendar allowlist).
**Still deferred** (unchanged from above): M3 per-principal *memory* partition
(M2 stopgap: `remember` is denied for Jr, so nothing writes to shared memory —
`recall` reads only), M4 org-scoped **relay** routing (until then, distinct
principals exist only for locally-provisioned direct tokens; the relay still
collapses remote callers to one shared token) and per-role child-safety **Narc**,
and the **COPPA** gate before any public under-13 account.
