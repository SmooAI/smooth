# ADR-009: Scoped cloud memory & knowledge — personal/shared tiers, guardian visibility, within-tool resource RBAC

- **Status**: Proposed
- **Date**: 2026-08-09
- **Pearls**: th-500765 (this ADR); Phase A th-b712fb, Phase B th-b6602f, M2.5 calendar th-716243, subscription th-74e0f8, platform family roles th-95eff9
- **Builds on**: [[ADR-008-family-ai-multi-principal]] (this is its deferred **M3**, expanded)

## Context

ADR-008 made Big Smooth multi-principal: several family members share one daemon,
each authenticated by their own token, each with role-scoped tool RBAC (M1+M2
shipped). It explicitly **deferred M3 — per-principal memory partition** — and
kept memory/knowledge single-tenant and local.

Two gaps remain, and a user request stacks a third:

1. **Memory is local, unsynced, unscoped.** `operator_storage`'s memory is one
   flat SQLite store on one machine (`memory_for_access(AccessContext)` receives
   the access context and _discards_ it). Nothing syncs across a user's faces
   (`th code`, web SPA, iOS, Android) — each is an island. There is no personal
   vs shared distinction.
2. **Platform knowledge is org-only.** api.smoo.ai knowledge is fully built —
   hybrid pgvector (Voyage 1024-dim) + BM25 + RRF, RLS by org membership — but
   scoped to `organization_id` and nothing finer for a _member_. There is no
   personal tier and no user/owner column anywhere in the knowledge schema.
3. **The ask**: cloud-synced memory and knowledge that can be **personal**,
   **family-shared**, or **org-shared**; children's data special-cased; and
   parents able to share only _certain_ local calendars with a child.

The load-bearing questions are **what "scope" means** (how many tiers, and how
they map to storage) and **where the boundary lives** (RLS + app filter, not
prompt text — same principle as ADR-008).

## Decision

### 1. Scope is one nullable owner column, not three stores

"personal / family / org" collapses to **two tiers on a single dimension**:

- `owner_user_id = <user>` → **personal**: visible only to that user, synced
  across _their own_ faces/devices.
- `owner_user_id = NULL` → **shared with the org**.

**Family = org (ADR-008), so "family-shared" and "org-shared" are the same tier** —
one is a household org, the other a company org. We do **not** build a separate
"family" scope. On the knowledge side the shared tier _already exists_ (it is
today's org-scoped knowledge); we are only adding the personal slice.

Every scoped row (knowledge documents + their vectors; memories + their vectors)
carries `organization_id` (exists) + `owner_user_id` (new, nullable). NULL on
migration = shared, which is the correct back-compat default for existing rows.

### 2. Guardian visibility — children are asymmetric

The parent rule: **a child's personal memory/knowledge is ALWAYS visible to
their parents.** So the read predicate is not symmetric `owner = me`:

```
visible(row, caller) :=
      row.owner_user_id IS NULL                          -- shared with org
   OR row.owner_user_id = caller                         -- my own personal
   OR (caller is an adult member AND row.owner is a child in this org)  -- guardian
```

Household default for the guardian clause: **any non-child member of the org is a
guardian of every child in it** — no per-child mapping to start. "Children are
special accounts" is encoded as _asymmetric visibility_, not a separate store.

**This clause needs the platform to know family roles** (who is a child), which
today live only in the daemon's local `family.toml` (ADR-008 M1/M2 kept them
local because the platform had no per-member family role). So:

- Phase A ships the **two-tier core** (personal invisible / shared) **without**
  the guardian clause.
- The guardian clause is a **single documented `WHERE`-extension seam** that
  lands once **platform family roles** (pearl th-95eff9) exist.

### 3. Cloud is optional, entitlement-gated, and degrades to local

Cloud sync is **opt-in** and gated behind a **Family AI subscription with a
14-day free trial** (pearl th-74e0f8, reusing smooai Stripe + entitlements). When
the org is not subscribed / trial expired, cloud sync is simply **off and the
daemon uses its existing local store** — i.e. today's single-tenant local
behavior, so a free user sees **no regression**, only the absence of sync. The
entitlement check is a gate on _whether the daemon talks to the cloud tier_, not
a new failure mode: local is always the floor.

### 4. Within-tool resource RBAC — calendars are the first case

ADR-008 RBAC is **tool-level** (a role gets `calendar` or not). Sharing _only
certain_ calendars with a child is **within-tool resource** RBAC: the child gets
the tool, scoped to a parent-approved subset.

- Config: `RoleProfile` (in `smooth-policy/src/family.rs`) gains an optional
  `calendars` allowlist. Absent = all (adult default); present = only those;
  empty = none. Fail-closed, same posture as `tool_allowed`.
- Enforcement: `SandboxedToolProvider::tools_for` already knows the principal's
  role (from `ctx.access.groups`) and constructs the local `calendar` tool. It
  **injects the allowlist into that tool instance for the turn**; the tool filters
  `list` output and rejects reads/writes targeting a calendar outside the set,
  Narc-visible. The allowlist is bound from the _principal_, not passed by the
  client — the child cannot widen it (same un-settable property as M2).
- Local only, matching the ask ("when the calendar skill is used locally") — the
  calendar tool is already macOS-local (shells `ical` outside the sandbox,
  th-94cc4a). No cloud involved.

This is a general pattern (role → resource allowlist injected at tool
construction); we build it for calendars because that is the request, and leave
the generalization to other tools for when there's a second case (YAGNI).

## Architecture

```
             smoo.ai platform (per org)
   knowledge_documents / knowledge_vectors   +  memories / memory_vectors   (Phase B)
   each row: organization_id (exists) + owner_user_id (new, nullable)
   read predicate: shared OR mine OR (adult sees child) [guardian = Phase A.2]
                     ▲  entitlement-gated (Family AI sub / trial)
                     │  REST: /organizations/{org}/knowledge/*  (+ /memories/* in B)
   ┌─────────────────┴───────────────────────────────┐
   daemon knowledge_search → `th knowledge search`     │  daemon remember/recall (Phase B)
   (already remote)                                    │  → platform, else LOCAL sqlite (fallback)
                                                       ▼
                              faces: th code · web SPA · iOS · Android  (one brain, many faces)

   LOCAL only (no cloud): calendar tool ← per-role calendar allowlist (M2.5)
```

## Reuse vs net-new

| Piece                                    | Today                                                  | Phase                    |
| ---------------------------------------- | ------------------------------------------------------ | ------------------------ |
| Knowledge shared (org) tier              | ✅ built — hybrid pgvector search, RLS by membership   | —                        |
| Knowledge **personal** tier              | ❌ no owner dimension                                  | **A** (th-b712fb)        |
| Guardian visibility (knowledge + memory) | ❌ needs platform family roles                         | **A.2**, dep th-95eff9   |
| Memory **cloud home** + scoping          | ❌ local flat sqlite; `memory_for_access` discards ctx | **B** (th-b6602f)        |
| Calendar allowlist (within-tool RBAC)    | ❌ tool-level only                                     | **M2.5** (th-716243)     |
| Optional / trial / entitlement gate      | ❌                                                     | subscription (th-74e0f8) |

## Alternatives rejected

- **Three parallel stores (personal / family / org).** A taxonomy where one
  nullable column suffices. Rejected — one `owner_user_id` gives both tiers, and
  family=org removes the third.
- **Family and org as distinct scopes.** ADR-008 already made a family _be_ an
  org; a separate family tier would be a second name for the same rows.
- **Dolt sync for memory** (pearls already sync via `refs/dolt/data`). Per-repo,
  single-namespace, no cross-user org-shared read model — and the ask is
  explicitly "on smoo.ai". The platform (which already has the pgvector muscle
  and a dormant `['memories', org, user]` / `['memories', org]` namespace sketch)
  is the right home.
- **Symmetric `owner = me` visibility.** Would hide a child's data from parents —
  violates the stated rule. Guardian asymmetry is required, not optional.
- **Making cloud mandatory / the default.** Rejected — cloud is opt-in and
  entitlement-gated; local single-tenant stays the free floor and the fallback.
- **Enforcing scope in prompt/persona text.** Same as ADR-008: scope is RLS + an
  app-layer `WHERE` filter, not prose.

## Consequences

- Knowledge and (later) memory schemas gain `owner_user_id`; migrations set NULL
  (shared) for existing rows — no data becomes accidentally private or public.
- Two enforcement points per read, because api-prime runs on a `BYPASSRLS` pool:
  the **RLS policy** (for direct Supabase access) _and_ the **app-layer filter**
  in the Rust handlers must both carry the predicate. Both get the guardian seam.
- M2M tokens (no user) see **shared only** — personal rows require a user identity.
- Memory recall quality _improves_ as a side effect of Phase B: the platform's
  hybrid vector search replaces the daemon's naive keyword scoring.
- Cloud memory unifies the faces (advances the `th code IS a face` epic
  th-d7366d) — but only when subscribed; unsubscribed daemons stay local islands.
- Guardian visibility is blocked on platform family roles (th-95eff9); shipping
  Phase A without it is deliberate and safe (personal-invisible is the strict
  default; the guardian clause only _widens_ parent reads later).

## Follow-ups (schema leaves room; behavior lands later)

Named now so the schema doesn't need re-migration:

- **Memory attribution** — a `source`/author column so a shared fact records
  _who taught it_ ("Mom said the wifi password is …"); trust + audit under
  multi-principal writes. (The pearls memory table already has `source`; the
  platform one will too.)
- **Child → shared-memory writes go through approval**, not straight in — route a
  child's proposed _shared_ write to a parent review queue (reuse the Hermes
  inbox-as-home, th-1f7fd7). Prevents a kid (or a prompt injection) poisoning
  shared memory.
- **Child-safety Narc on memory content** — the per-role child-safety detectors
  ADR-008 already deferred, now also gating what gets _remembered_.
- **Scoped forget / COPPA wipe** — parent-triggered deletion of all of a child's
  memory + knowledge. A hard legal prerequisite the moment a real under-13
  account exists (see ADR-008 §COPPA).
- **Connected-account personal memory** — Gmail/calendar MCP feeding
  _personal_-scoped auto-memory ("remember my dentist appointments").

## Rollout

1. **ADR-009** (this) — the contract; `owner_user_id` + attribution columns are
   decided now so Phases A/B don't re-migrate.
2. **Phase A** (th-b712fb) — knowledge personal tier: `owner_user_id` on
   documents+vectors, RLS + app-filter two-tier read, write-path owner setting,
   guardian `TODO` seam. Brent-gated deploy.
3. **M2.5** (th-716243) — calendar allowlist, local, no deploy. Independent of
   A/B; can land any time after this ADR.
4. **Platform family roles** (th-95eff9) — unblocks the guardian clause in A + B.
5. **Phase B**
    - **B.1** (th-b6602f) — memory cloud home reusing A's owner/guardian pattern
      (`memories` REST: create/search/list/delete, owner-tiered). **Shipped to
      smooai main.** Brent-gated deploy.
    - **B.2** (th-5189f0) — daemon `remember`/`recall` routing to B.1 with a
      **fail-soft local fallback** (`smooth-daemon`: `CloudMemory` +
      `StorageAdapter::memory_for_access`). **Opt-in, default OFF** behind the
      `SMOOTH_CLOUD_MEMORY` env toggle (`1`/`true`/`yes`/`on`) so there is zero
      behavior change until it is enabled AND B.1 is deployed. Writes default to
      the personal tier; an entry opts into shared via a `scope="shared"` memory
      metadata key. No entitlement gate here (that is step 6).
6. **Subscription** (th-74e0f8) — Family AI bundle + 14-day trial; the
   entitlement gate + local-fallback wiring. Brent/billing-owned deploy.
