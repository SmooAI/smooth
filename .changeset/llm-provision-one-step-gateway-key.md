---
'@smooai/smooth': minor
---

`th llm onboard` — connect Big Smooth to a Smoo AI org in one step.

Big Smooth is something an org adopts, but there was no adoption path. The three
pieces existed and nothing joined them: `th auth login` for the Smoo session,
`th llm keys create <name>` to mint a LiteLLM virtual key whose value prints
**exactly once**, and `th model login` to write provider credentials into
`~/.smooth/providers.json`. Joining them meant a human copying a live credential
out of their terminal and pasting it into a second command.

`th llm onboard` (alias of `th llm provision`) does the whole path: reuse the
Smoo session — signing in if there isn't one — **choose the org**, mint
`big-smooth-<hostname>` on it, back up `providers.json`, write the key into the
`smooai-gateway` provider with every routing slot pointed at it, and make one
real call through the gateway to prove the key works. **The key value never
touches the terminal** — the only time it is printed is when the write failed, in
which case the value is live, billable, and unrecoverable, so losing it silently
is the worse outcome.

**Big Smooth then bills to the org that onboarded it.** The key is minted on that
org, so its LiteLLM team and budget are what Big Smooth spends against
(`team_id == org_id`) — per-org billing falls out of the existing model with no
special-casing. That is precisely what is broken today: Big Smooth runs on the
master org's internal `__backend__` key, sharing a team with smoo.ai's public
chat agent, and in 2026-08 it reached 95.3% of that team's budget and took the
public agent down with it.

- **The org is chosen, never defaulted.** `--org-id` takes a UUID or a name/slug
  substring; omit it and you get the same interactive picker `th org switch`
  uses; with no TTY it is a hard error. Silently taking the active org would bill
  Big Smooth to whichever customer you last looked at.
- **Goes through the existing org-admin-gated API** as the logged-in user — no
  LiteLLM admin key, no side path. A 403 is reported as "connecting Big Smooth to
  <org> needs ADMIN on that org", not a raw status line.
- **Idempotent as "already connected"**, not "key exists": a re-run recognises the
  org and offers `--rotate` (which is also how a second machine gets a value)
  rather than stacking keys.
- Big Smooth mints its **own named key** rather than the org's single `default`
  key — an org may already have a gateway key for its own reasons, and "the org
  has a key" is not "Big Smooth is onboarded". It also keeps Big Smooth's spend
  separable in `LiteLLM_SpendLogs`. Same team, so same budget: attribution, not
  isolation.
- Minting is **not read-only** — the route runs `syncOrgLlmLimits` first ("no cap,
  no key"), re-stamping the org's tier budget onto its team. That is the exact
  mechanism behind the outage above, so the output says it happened.
- Routing is written with **concrete** model names: the preset still spells the
  slots with the `smooth-*` aliases retired at the gateway (SMOODEV-1793), so they
  are migrated before anything reaches disk.
- `--credential-only` stores the key and leaves the user's default provider and
  routing alone; `--no-verify` skips the proof call and says so in the output.

Not shipped, both needing a smooai-side route change: a per-key `max_budget` +
budget window (`POST /organizations/{org}/llm-gateway/keys` accepts `{ name }` and
nothing else, so the flag would be wired to nothing), and reporting the resulting
budget cap (no API surface returns it). When the cap does ship, the **window must
ship with it** — `max_budget` with no `budget_duration` is a lifetime cap that
never resets, which is how a monthly-sized number becomes an outage.
