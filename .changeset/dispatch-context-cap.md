---
'@smooai/smooth': patch
---

smooth-operative: cap the injected project-context and workspace-memory system-prompt blocks at 16 KB each. The engine already discovers and injects project context files (`~/.smooth/CONTEXT.md`/`AGENTS.md`/`CLAUDE.md`, then `<repo>/.smooth/CONTEXT.md` → `SMOOTH.md` → `AGENTS.md` → `CLAUDE.md`) plus `.smooth/MEMORY.md`, but the consumer injected those unbounded — a giant AGENTS.md/README could crowd out the context window. Each block is now truncated on a UTF-8 char boundary with a `[... truncated ...]` marker (pearl th-5002c4). Documented in `docs/Operations/Running-Locally.md`.
