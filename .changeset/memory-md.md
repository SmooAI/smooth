---
"@smooai/smooth": minor
---

Per-workspace agent memory at `.smooth/MEMORY.md` (cold-start orientation)

Cold-start agents land in `/workspace` with **zero idea what the
project is**. The runner used to load `AGENTS.md` (if present) into
the system prompt and that was it. For a fresh repo without
AGENTS.md, the agent had no signal — it would guess from the
question's phrasing and hallucinate ("you mentioned dev server,
must be Rust") when the repo turned out to be Next.js.

Adds a writeable per-workspace memory layer the agent maintains
itself across sessions:

- **`.smooth/MEMORY.md`** — auto-loaded into the operator system
  prompt at startup as a `## Workspace Memory` section, alongside
  the existing `## Project Context` from AGENTS.md. Empty / missing
  is fine; the system prompt tells the agent to populate it.
- **`read_memory` tool** — returns the current contents (empty
  string when the file doesn't exist yet). Always cheap; intended
  to be called before answering any project-specific question.
- **`write_memory` tool** — `mode='append'` (default) adds a
  section to MEMORY.md separated by a blank line; `mode='replace'`
  overwrites the entire file. Both modes create `.smooth/` if
  missing.
- **Allowed for all roles**, including the read-only ones (oracle,
  mapper, heckler, scout). Memory is metadata, not source code;
  persisting findings is part of being a good cohabitant. (Scout
  gets `read_memory` only — sidekicks return summaries, not
  durable journal entries.)
- **System-prompt discipline** — new "Memory & orientation"
  section in `prompts/system.md` codifies the loop:
    1. Assess. Do I actually know what this project is?
    2. Check loaded context first — `## Workspace Memory`,
       `## Project Context`. No tool call needed.
    3. If gaps remain, explore — `list_files` + read marker
       files (`README.md`, `package.json`, `Cargo.toml`, etc.).
    4. Persist what you learned — `write_memory` with terse
       bullets so the next session inherits.
    5. Re-check periodically — on long tasks, every several
       iterations, ask "have I learned something durable?"

Effect: a "how do I run dev mode here" question on a fresh repo
now goes (1) `read_memory` → empty → (2) `list_files` →
`read_file package.json` → (3) answer + `write_memory` with the
findings. Next session, (1) `read_memory` returns the bullets and
the agent can answer without exploring.
