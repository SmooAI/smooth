---
name: create-skill
description: Author a new skill (SKILL.md) for Smooth. Asks clarifying questions, drafts the frontmatter + body, writes the file to the user's chosen location, and offers a test invocation.
triggers:
  - make a skill
  - create a skill
  - add a skill
  - save this as a skill
  - new skill
  - author a skill
scope: host
allowed_tools:
  - read_file
  - write_file
  - edit_file
  - list_files
  - bash
---

# create-skill

The user wants to add a reusable recipe to their Smooth setup. Your job: turn a description of what the recipe should do into a well-formed `SKILL.md` file at the right path.

## Process

### 1. Clarify (if needed)

If the user's request is vague — e.g. "make a skill for git stuff" — ask ONE question to narrow it:

- "What should this skill do specifically? Concrete steps help."

Skip clarifying if the request is concrete enough on its own ("make a skill that adds a movie to my smoo-hub watchlist using the api at smoo-hub:8787" — that's actionable).

### 2. Decide scope: project or user

Ask if you don't know:

- **Project scope** (`<workspace>/.smooth/skills/<name>/SKILL.md`) — the skill is tied to this codebase. Other workspaces don't see it. Commit it to the repo so teammates get it too.
- **User scope** (`~/.smooth/skills/<name>/SKILL.md`) — the skill applies to every Smooth dispatch you ever do, in any workspace. Personal.

Default to user scope if the user just says "save it" without specifying.

### 3. Pick a name

Lowercase, hyphenated, descriptive. `add-show`, `format-rust`, `sync-to-s3`. The directory name and the `name:` frontmatter must match.

### 4. Determine the scope: sandbox or host

- `sandbox` (default) — the skill runs inside the microVM. Use when the skill only touches `/workspace`, runs build/test commands, edits source code, or needs nothing outside the sandbox.
- `host` — the skill bypasses the microVM and runs in Big Smooth's process directly. Use ONLY for genuine host-needing cases: `scp` to a local-network host, `sips` / macOS-specific tools, AWS SSO browser flows, Photos.app integration.

**Network alone is NEVER a reason for `host`.** Network access from the sandbox is handled by `allowed_hosts` below.

### 5. Determine `allowed_hosts`

If the skill needs to reach a host the default Wonk policy doesn't allow (`llm.smoo.ai` is the only default), list those hosts here. Examples:

- `smoo-hub` — LAN/tailscale-only personal server
- `api.tvmaze.com` — public API
- `*.azureedge.net` — wildcard for a CDN family

Be specific. Don't list `*` or "all"; users won't accept that grant.

### 6. Determine `allowed_tools`

Optional. If left empty, the skill inherits the agent's full toolset. Use to RESTRICT (not expand) — e.g. a read-only summarize skill might say `allowed_tools: [read_file, list_files, grep]`.

### 7. Write the SKILL.md body

The body is what the agent reads when the skill is invoked. Make it:

- **Short.** 30–80 lines for most skills. A long skill that the model has to wade through is worse than no skill.
- **Step-by-step.** Numbered list of what to do, in order. The model will follow it literally.
- **Concrete on commands.** Show the exact `curl`, `bash`, or tool invocation. Not "make an API call" but `curl -X POST http://smoo-hub:8787/api/shows -H 'Content-Type: application/json' -d '{...}'`.
- **Explicit on inputs.** Name what the user will provide (title, status, etc.) and what defaults you'll assume when they're missing.

Optional sections worth including:

- `## Inputs` — what the user typically provides
- `## Outputs` — what the user will see / what gets created
- `## Failure modes` — what to do when X is missing, Y returns 404, etc.

### 8. Write the file

For project scope:
```bash
mkdir -p .smooth/skills/<name>
# write SKILL.md
```

For user scope:
```bash
mkdir -p ~/.smooth/skills/<name>
# write SKILL.md
```

Then run `th skills list` to confirm the skill is discovered.

### 9. (Optional) Test it

If the user wants, offer to invoke the skill once with a sample input. Just suggest the invocation phrasing — don't auto-invoke unless they ask.

## Output

When done, reply with ONE sentence:

> "Created `<name>` at `<path>`. Run `th skills show <name>` to inspect or invoke by saying something matching: `<one trigger phrase>`."

That's it. No essay. The diff is the artifact; the sentence confirms it landed.
