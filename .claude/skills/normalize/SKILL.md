---
name: normalize
description: Audit the `th` CLI (crates/smooth-cli) for resource-noun command groups missing their singular/plural counterpart alias, and add the missing clap `visible_alias` attributes so either form works. Use when adding a new `th` command group, or on the phrases "/normalize", "normalize the CLI", "plural singular aliases", "why doesn't `th agent` work".
---

# normalize — singular ⇄ plural command aliases

Every resource-noun command GROUP in `th` should accept both its singular and
plural form (`th api agents` ⇄ `th api agent`, `th testing runs` ⇄ `th testing
run`). This skill finds the gaps and adds the aliases.

## Rules (do not violate)

1. **Resource-noun command GROUPS only** — a variant that holds a
   `#[command(subcommand)]` (a container of CRUD verbs). NEVER alias leaf verbs
   (`list`, `create`, `run`, `search`), positional args, gerunds (`testing`),
   or acronyms (`crm`). `th run` (execute a pearl) is a verb — leave it alone;
   `th testing runs` (test-runs resource) gets the `run` alias.
2. **Aliases only, never rename.** Add `#[command(visible_alias = "...")]`; keep
   the canonical variant name and its command spelling exactly as-is.
3. **`visible_alias`** (shows in `--help`), not `alias` (hidden).
4. **Skip collisions.** If the counterpart form is already a distinct command
   in the same enum, skip it. Also skip *semantic* collisions: e.g. top-level
   `th agent` is agent-messaging while `th api agents` is agent CRUD — do NOT
   alias top-level `agent`→`agents`; it would make `th agents` mean messaging.

## Audit

```bash
python3 .claude/skills/normalize/audit.py         # prints OK / GAP / SKIP-collision per group
python3 .claude/skills/normalize/audit.py | grep GAP
```

The auditor works off a curated resource-noun pair list (`PAIRS` in the script)
— algorithmic pluralization is deliberately avoided (it invents "childrens",
"bulk-sets", verb-plurals). Adding a new resource = add its `(plural, singular)`
pair. Output columns: `STATUS  file  Enum  command -> counterpart`.

Gap table to report (one row per gap):

| command | canonical | missing alias | collision? |
|---------|-----------|---------------|------------|

## Fix pattern

Insert the attribute directly above the group variant (keep the doc comment):

```rust
/// Smoo AI async job queue.
#[command(visible_alias = "job")]     // <- add this line
Jobs {
    #[command(subcommand)]
    cmd: smooai::jobs::Cmd,
},
```

Applying is a hand edit (the doc comment is the unique anchor); the script
audits, it does not rewrite source. Sweep both `Cmd` enums in `main.rs`
(`Commands`, `ApiCommands`) and every `crates/smooth-cli/src/smooai/*.rs`
subcommand enum.

## Verify

```bash
cargo test -p smooai-smooth-cli cli_definition_is_valid   # clap debug_assert — catches alias collisions
python3 .claude/skills/normalize/audit.py | grep GAP      # should be empty for the swept scope
```

Add a parse assertion for a sample of new aliases in `main.rs`'s
`org_cli_tests` module (see `singular_plural_aliases_parse`).
