# The `th` CLI Spec

> The codified interface contract for the `th` binary (and its `smoo` alias).
> Pearl th-7f1da8. Enforced where possible by tests in
> `crates/smooth-cli/src/help.rs` (`help_sync_both_directions`,
> `spec_every_platform_list_has_json`); everything else is reviewed against
> this page. When a rule and the code disagree, one of them is a bug — fix or
> amend deliberately, never drift silently.

## 1. One binary, two products

- **`th`** is the standalone local agent toolbox: pearls, worktrees, agent
  mail, the daemon, attest, the coding TUI. No account required, works
  offline.
- **`smoo`** (a symlink to the same binary; argv[0] dispatch) is the Smoo AI
  platform CLI: everything that talks to smoo.ai lives under this namespace,
  and all of it authenticates via `smoo auth login`. `smoo X` ≡ `th smoo X`.
- Old pre-namespace spellings (`th api …`, `th config`, …) parse as **hidden
  compat aliases**. They are load-bearing for existing docs; never remove one
  without a sweep, never document one in new material.

## 2. Command shape: noun, then verb

```
th   <noun> <verb> [args] [flags]        # local tool
smoo <noun> <verb> [args] [flags]        # platform
```

- **Nouns are resource groups** (`pearls`, `crm`, `org`, `files`). Singular
  and plural both parse — declare the canonical form and add the other via
  `visible_alias` (the `/normalize` skill audits this). Exception: bare
  `th agent` (mailbox registry) vs `smoo agents` (platform agents) — the two
  trees keep the collision resolved; see Using-th-CLI §1a.
- **Standard verbs**, in this order of preference when naming a new one:
  `list`, `show`, `create`, `update`, `rm`, `search`. Don't invent synonyms
  (`get` for `show`, `delete` for `rm`) inside one group; when a sibling group
  already shipped the synonym, alias it rather than diverging further.
- A group whose most common action is obvious may make it the **bare
  default** (`smoo workforce` → `directory`), implemented as an
  `Option<Cmd>`, never by duplicating a verb.

## 3. Standard flags

| Flag | Rule |
|---|---|
| `--json` | **Every read verb** (`list`/`show`/`search`/reports) offers it. Output is the raw response JSON, stable, unstyled, no truncation beyond what the server did. Enforced by test for every `list` under `smoo`. |
| `--org-id` (alias `--org`) | Every platform verb that acts on an org accepts an override; default is the active org (`smoo org switch`). |
| `--profile` | Global; selects the auth profile. Never define a per-command flag with this name. |
| `--confirm` | Required for any action that **sends, spends, or destroys** (`smoo campaigns send`). The unflagged invocation previews (server-side dry-run where the API offers one) and says exactly what `--confirm` would do. |
| `--dry-run` | For mutations where a preview needs to be explicit rather than the default. Prefer preview-by-default + `--confirm` for the dangerous class above. |

## 4. Output contract

- **Empty is an answer.** "No campaigns matched. This is a confirmed read of
  the campaign list, not a read failure." — never exit non-zero, never print
  nothing, never phrase an empty result as an error.
- **Truncation is always reported** ("showing 50 of 1,126 — …").
- **Errors are two lines**: what failed (with the server's message verbatim
  when there is one), then what to do next. Never a bare status code, never a
  backtrace at a user.
- **Results on stdout, progress/notes on stderr**, so redirection works.
- Numbers/timings/money right-align in tables; state carries a **glyph**
  (`●`/`○`/`◐`) as well as a color, so meaning survives `NO_COLOR`.

## 5. Color (Presence)

Full language: `.claude/skills/smooth-glow-up`. The CLI rules:

- **Pipe-safe always**: every styled print goes through `anstream` (or clap's
  own detection). `th … | grep` must never see an escape code; `NO_COLOR=1`
  is honored. The integration test `no_ansi_when_piped` guards this.
- The **teal→blue gradient is the `th` wordmark** and Big Smooth's presence —
  it appears on the wordmark and nowhere else. The orange→pink gradient is
  the `Smoo` brand half. Neither is ever used for chrome, headers, or
  emphasis.
- One accent: **teal** on command/flag literals (clap `brand_styles()` in
  main.rs and the curated help). Headers bold. Secondary text dimmed. Amber
  is reserved for "Big Smooth needs you" and nothing else.
- `--json` output is never styled.

## 6. Help

- **Bare `th --help`** renders the curated, grouped screen (`help.rs`
  `SECTIONS`) — a map, not a manual. A two-way sync test pins it to the clap
  tree: adding a command without adding it to a section fails CI.
- `th --help-full` prints clap's native flat tree. Per-command `--help` is
  clap-native, themed by `brand_styles()`.
- Doc comments on commands: **first line ≤ ~70 chars** (it's the summary in
  every listing), blank `///` line, then detail. Long prose belongs in the
  detail block or the docs, not the summary.

## 7. The `ai` explainer

Append `ai` to any command path for a plain-markdown guide generated from the
clap tree plus curated examples:

```
th ai              # the whole binary
smoo ai            # the platform namespace
smoo org ai        # one group: about, subcommands, flags, examples, conventions
```

- Output is **unstyled markdown**, written to be handed to an AI agent as
  much as read by a human.
- It is generated — new subcommands appear automatically. Only
  `help.rs::EXAMPLES` is hand-curated; add a row when a worked example
  genuinely helps.
- The word `ai` only triggers when every preceding segment resolves to a real
  command, so positional values are unaffected.

## 8. Adding a surface (checklist)

1. Platform resource → module under `crates/smooth-cli/src/smooai/`,
   registered in `SmooCommands` (+ `ApiCommands` if it's route-shaped);
   local tool → top-level `Commands`.
2. Clone the nearest sibling's shape; use the shared helpers
   (`print_json`, `require_authed`, `require_active_org`, `UserClient`).
3. Verbs + flags per §2–§3; output per §4; colors per §5.
4. Add the command to a `help.rs` section (the sync test will remind you).
5. Colocated `#[cfg(test)]`: parse tests for every verb, unit tests for
   rendering/logic. Live-smoke reads before shipping; never live-fire writes.
6. Docs: `docs/Engineering/Using-th-CLI.md` + help text. Changeset.

## 9. Mirrors

The hosted MCP server (mcp.smoo.ai) and this CLI are twins over the same
routes: every MCP read tool has a CLI verb (PR #488) and both follow §4's
empty/truncation rules. When adding to one surface, add to the other in the
same effort or file the pearl for it immediately.
