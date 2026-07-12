---
status: Accepted
date: 2026-07
deciders: Brent
supersedes: None
superseded-by: None
tags: [decision, security, auth, product, crawl]
---

# ADR-005: Publishable-client + scoped-org auth for the `th` web-crawl free tier

#decision

**Date**: 2026-07
**Status**: Accepted (implemented SMOODEV-2564, 2026-07-12)
**Pearl**: th-91f158 (this ADR); blocks th-1d88f5 (search.smoo.ai implementation)

> **Implementation note (SMOODEV-2564).** The free tier shipped on the existing
> api-prime + in-house-crawler seam (ADR-035), not on search.smoo.ai (th-1d88f5,
> still future). The bundled credential is realized as a **rotatable, non-secret
> client-id header** (`x-crawl-client-id`, allow-listed via the
> `crawlerPublicClientIds` config key) on a **pure `auth:['public']` route**
> `POST /crawl/scrape` — rather than a real org JWT scoped to a dedicated org.
> This delivers the same four properties more simply: the credential is public
> (§Decision 1), its blast radius is exactly the crawl route because there is **no
> org behind a public route at all** (§Decision 1 — stronger than a scoped org),
> it's rotatable via the allow-list (§Decision 4), and abuse is bounded by an
> **in-handler per-client-IP per-minute + per-day quota** keyed on
> `x-forwarded-for` (§Decision 2 — the edge limiter only sees the ingress IP for a
> public route, so the real quota lives in the handler). The free tier is also
> **static-only**: `render` is forced to `never` (no headless Chrome) and
> `extract`/`summary` (billed LLM) are rejected — those need a login (paid tier,
> §Decision 3). A dedicated org + role (§Implementation) was therefore
> unnecessary for the crawl route; keep it in mind if a future public tier ever
> needs to touch org-scoped data.

## Context

We want to ship a web-crawl / scrape service — Smooth's take on Firecrawl —
that turns any URL into agent-ready markdown/JSON. The backend is
[[../../README|search.smoo.ai]] (pearl th-1d88f5: EKS Rust proxy,
Brave/Exa/Parallel routing, Redis cache, JWT org-scoping — not yet built).

The product ask is a **free tier that works the moment you install `th`**, with
no signup, plus **paid tiers** for the volume real crawling needs. "Works the
moment you install" implies a credential that ships inside the distributed `th`
binary.

The tempting shortcut — bake a normal M2M key for a Smoo AI prod org into the
binary — is a security hole:

- **A key in a distributed binary is not a secret.** Anyone can `strings` it
  out. Any control that assumes the key is confidential is already defeated.
- **A normal M2M key inherits its org's full permissions.** If it's a real prod
  org, whoever extracts the key can reach that org's CRM, config, knowledge
  base — everything the org can — not just crawl. The blast radius is the whole
  org.
- **A free endpoint behind a shared key is a cost/DoS magnet.** We pay upstream
  crawl + egress for every anonymous caller; abuse control cannot depend on the
  key being hard to obtain.

We already run a public-client pattern in production: the chat-widget's public
agents authenticate by `Origin` against `authPublicClientAllowedOrigins`
(CLAUDE.md §11) — a deliberately narrow, non-secret client, not a general org
key. This ADR generalizes that stance for the crawl free tier.

## Decision

Treat the bundled credential as a **publishable client identifier, not a
secret**, and enforce a hard scope boundary:

1. **Dedicated, minimally-scoped org + role.** The bundled client belongs to a
   purpose-built org whose role can reach **only the crawl free-tier route** —
   never CRM, config, knowledge, or any other `api.smoo.ai` surface. Extracting
   the key gets you exactly the free crawl tier and nothing else.

2. **Free tier = the bundled public client, throttled at the edge.** Rate limit
   and quota **independent of key secrecy** — keyed on client IP and a
   per-install id, not on "the key is hard to get." The bundled key only asserts
   "this caller is the `th` tool," which selects the free tier and its limits.

3. **Paid tiers = real identity, never the shared key.** Pro/Enterprise
   authenticate with the caller's **own org identity** — `th api login` → org
   JWT, or a per-customer minted M2M key — metered and billed per org
   (th-1d88f5's JWT org-scoping is exactly this seam). The public key never
   unlocks a paid tier.

4. **Rotatable, version-allow-listed.** The bundled client is versioned and the
   backend allow-lists more than one, so we can rotate it (revoke abused
   versions, ship a new one) without bricking older `th` builds still in the
   field.

## Reasoning

### Scope isolation over key secrecy

We cannot make the bundled key secret, so we stop relying on secrecy and make
the key *worthless beyond the free crawl route*. A minimally-scoped org caps the
blast radius at "free crawl," which is a tier we were giving away anyway.

### Abuse control at the edge, not at the key

Because every free caller presents the same key, the key can't be the rate-limit
subject. IP + install-id quotas, plus optional proof-of-work / captcha on
anomalous spikes, bound the cost we eat. This is the only defense that survives
the key being public.

### Reuse the identity seam we're already building

search.smoo.ai (th-1d88f5) is already designed around JWT org-scoping. Paid
tiers ride that seam directly — the free public client is the one addition, and
it's a narrow one.

## Implementation

- **Backend** (`search.smoo.ai`, th-1d88f5): route-level authz that accepts the
  public client only for `POST /crawl` (or equivalent) at free-tier limits;
  paid limits gated on a real org JWT / M2M. Edge rate-limit + quota by
  IP/install-id in Redis.
- **Org/role provisioning**: a dedicated `th-public-crawl` org (name TBD) with a
  role scoped to the crawl route only. Provision via `th admin`
  (th-feebd2) once that surface lands; interim: mint manually.
- **`th` binary**: embed the public client id/secret (build-time), send it on
  free-tier crawl calls; on `th api login`, prefer the user's org JWT and switch
  to paid limits automatically.
- **Config**: the public client id is not a secret but still flows through
  `@smooai/config` / `th config` as a first-class key — no hardcoded scatter
  (CLAUDE.md §16).
- **Rotation**: client id carries a version; backend allow-lists a small window
  of versions.

## Consequences

### Positive

- Free tier works on install with zero signup, and a leaked key exposes only
  free crawl — never org data.
- Paid conversion is a clean auth upgrade (`th api login`), not a separate
  integration.
- Cost exposure is bounded by edge quotas we control, not by key obscurity.

### Negative

- We eat the free-tier crawl + egress cost for anonymous users; edge limits must
  be tuned conservatively and watched.
- Rotation is operational overhead (version window, revocation) we own forever.
- A determined abuser can still burn a single install's free quota; per-IP
  limits mitigate but don't eliminate distributed abuse.

### Neutral

- Marketing may show the tiers before the endpoint is GA — copy must label it a
  preview with proposed (not final) limits/prices.
- Final free-tier limits and Pro pricing are commercial decisions, out of scope
  for this ADR (it fixes the *auth model*, not the numbers).
