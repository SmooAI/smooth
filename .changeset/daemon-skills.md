---
"@smooai/smooth": minor
---

Big Smooth daemon gains a skills capability (th-daemon-skills) — discover + use skills, and author its own.

- **Skill discovery + surfacing** (`crates/smooth-daemon/src/operator.rs`): at
  agent-build time the daemon reuses `smooth-cast`'s canonical skill discovery
  (project `.smooth/skills`, `~/.smooth/skills`, `~/.claude/skills`, builtins) and
  folds a concise "Available skills" index — name + description + triggers + the
  SKILL.md path — into Big Smooth's persona. Progressive disclosure: bodies are
  NOT dumped; the agent `read_file`s a SKILL.md only when a request matches.
  Empty discovery injects nothing; a malformed SKILL.md is skipped, never crashes.
- **`create_skill` tool** (`crates/smooth-tools/src/create_skill.rs`): lets the
  agent author its own reusable skills. Writes a well-formed
  `~/.smooth/skills/<name>/SKILL.md` (YAML frontmatter serialized with the same
  `serde_yml` the catalog parses back — lossless round-trip). Kebab-case name
  validation rejects path traversal; refuses to overwrite an existing skill
  unless `overwrite: true`; atomic write, no shell. Registered in the daemon's
  default tool set.
