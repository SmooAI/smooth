---
name: smooth-operator
description: Drive the user's Smoo AI org Smooth Operator from the CLI via `th api smooth-operator chat` — draft/send email, create/search CRM contacts, generate templates, query analytics, search the knowledge base. Use when the user asks for an org-level action ("email this lead", "add a contact", "how many conversations last week", "draft a follow-up") rather than a code change. Handles the confirm-before-execute flow for destructive actions (email send) safely.
---

# smooth-operator — drive the org's dashboard agent from `th`

The Smoo AI **Smooth Operator** is the always-on agent inside the SmooAI dashboard.
It acts on the *org's own* data on behalf of an operator: searching the
knowledge base, looking up and creating CRM contacts, querying analytics,
generating content/templates, and drafting + (on confirmation) sending email.
`th api smooth-operator` is the headless bridge to it — reach for it when the user
wants an **org action**, not a code change.

## When to use this

Use `th api smooth-operator chat` when the ask is an org operation, e.g.:

- "Draft a follow-up email to jane@acme.com" → smooth-operator drafts it
- "Send that email" → smooth-operator pauses on a **destructive** action (see confirm flow)
- "Add John Doe (john@acme.com) as a contact" → `crm.create_contact`
- "Find contacts named Jane" → `crm.search_contacts`
- "How many conversations did we handle last week?" → `analytics.ask`
- "Generate a welcome-email template" → `templates.generate`
- "What does our knowledge base say about refunds?" → `knowledge.search`

Do **not** use it for code changes, deploys, or anything the other `th`
subcommands already do (`th pearls`, `th worktree`, `th api agents`, …).

## Auth

Smooth Operator routes are user-authed. Run `th auth login` once (Supabase user
session); the CLI auto-refreshes. It 401s under an M2M client. The org is the
active org, or pass `--org <id>` / set `SMOOAI_ORG_ID`.

## The commands

```bash
# Start a conversation (prints reply + a compact "ran <tool>" line per tool call)
th api smooth-operator chat "Find contacts named Jane and draft a follow-up email"

# Continue it — pass the conversationId from the previous turn
th api smooth-operator chat "Make it warmer" --conversation <conversation-id>

# Machine-readable turn result (conversationId, reply, toolCalls, pendingAction)
th api smooth-operator chat "..." --json

# Read a conversation back
th api smooth-operator history <conversation-id>
```

## The confirm flow (destructive actions — READ THIS)

Destructive tools (currently **`email.send`**) never auto-run. When a turn
triggers one, the response carries a `pendingAction` and the loop pauses. How
`th` resolves it:

- **On a TTY (a human is watching):** you get a `Approve email.send — …?` y/N
  prompt. Default is **No**.
- **Non-interactively (you, the agent):** you must decide *up front* with a flag:
  - `--confirm` — auto-approve any destructive action this turn triggers
  - `--no-confirm` — auto-decline it
  - **no flag** → `th` prints the pending action and **stops without running
    it** (it does not guess). This is the safe default.

**Rules for agents:**

- **Never pass `--confirm` by default.** Only add it when the user has
  explicitly authorized *this* send (e.g. "yes, send it").
- To inspect first, then decide, run the chat **without** a flag, read the
  printed `pendingAction` (or use `--json`), and — if authorized — confirm the
  *same* conversation without resending the message:

  ```bash
  th api smooth-operator chat "Send the follow-up to jane@acme.com"   # pauses, prints the pending email.send
  # ...user confirms they want it sent...
  th api smooth-operator confirm <conversation-id> --approve          # runs it
  th api smooth-operator confirm <conversation-id> --decline          # or drop it
  ```

- When the user has already said "send it" in the same breath, the one-shot
  form is fine:

  ```bash
  th api smooth-operator chat "Send jane@acme.com the follow-up email" --confirm
  ```

## Notes

- Responses are **buffered** (no token streaming yet — phase 2 on the backend).
- Each tool run is audit-logged against the logged-in user.
- Tool availability is gated by org entitlements (e.g. CRM tools need the `crm`
  feature; `email.send` needs a configured email integration).
