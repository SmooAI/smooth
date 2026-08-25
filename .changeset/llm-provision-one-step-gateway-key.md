---
'@smooai/smooth': minor
---

`th llm provision` — one step from "logged out" to "Big Smooth has a working gateway key".

The three pieces already existed and nothing joined them: `th auth login` for the
Smoo AI session, `th llm keys create <name>` to mint a LiteLLM virtual key whose
value prints **exactly once**, and `th model login` to write provider credentials
into `~/.smooth/providers.json`. Joining them meant a human copying a live
credential out of their terminal and pasting it into a second command.

`th llm provision` does the whole path: reuse the session (or sign in if there
isn't one) → mint `big-smooth-<hostname>` → back up `providers.json` → write the
key into the `smooai-gateway` provider and point every routing slot at it → make
one real call through the gateway to prove the key works. **The key value never
touches the terminal** — the only time it is printed is when the write failed, in
which case the value is live, billable, and unrecoverable, so losing it silently
is the worse outcome.

- Idempotent: an existing key of that name is a visible skip, never a second
  billable key stacked behind the same label. `--rotate` mints a fresh value.
- Routing is written with **concrete** model names — the preset still spells the
  slots with the `smooth-*` aliases retired at the gateway (SMOODEV-1793), so
  they are migrated before anything reaches disk.
- `--credential-only` stores the key and leaves the user's default provider and
  routing alone; `--no-verify` skips the proof call and says so in the output.

**A new key gives attribution, not isolation** — the help text says this, because
the incident that motivated the command was the opposite assumption. LiteLLM
enforces budget at the _team_ level and the team is the org, so every key in one
org shares one budget: Big Smooth spent 95% of the master org's lifetime budget
and took smoo.ai's public chat agent down with it. `--org-id <other-org>` is the
only actual boundary.

Not shipped: a per-key `max_budget` + budget window. The
`POST /organizations/{org}/llm-gateway/keys` route accepts `{ name }` and nothing
else, so a `--max-budget` flag here would be a switch wired to nothing. It needs
the smooai-side route to pass `maxBudget`/`budgetDuration` through to
`createVirtualKey` first — and when it does, the window must ship with the cap:
`max_budget` with no `budget_duration` is a **lifetime** cap that never resets,
which is the exact shape of the outage above.
