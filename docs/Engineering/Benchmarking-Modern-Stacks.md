# Benchmarking Modern Stacks

#engineering

> [!info] Two halves of one loop
> **Steering** pushes the agent toward current practice. **Measurement**
> tells you whether the steering worked. Neither is worth much alone —
> steering you cannot measure is a vibe, and a benchmark you cannot act
> on is a scoreboard.

## The failure this is about

On a current stack the differentiating failure is not "can it write
React". Every model can write React. It is that a model trained on
2023–2024 documentation confidently writes 2023–2024 idioms. The code
compiles, reads well, and an LLM judge happily calls it good — and it is
wrong for the stack we actually ship.

Grounded in `smooai/apps/web` (next 16.2, react 19.2, tailwindcss 4.1,
`@tanstack/react-query` 5.72, `@tanstack/react-table` 8.21):

| stale idiom | why it is wrong now |
| --- | --- |
| `tailwind.config.js` | Tailwind 4 is CSS-first — tokens live in `@theme` |
| `@tailwind base;` | v3's three directives; v4 is one `@import "tailwindcss"` |
| `getServerSideProps` | pages-router data fetching, removed in the App Router |
| `forwardRef` wrapper | React 19 passes `ref` as an ordinary prop |
| `useQuery(key, fn)` | TanStack Query v5 takes a single options object |
| `cacheTime` | renamed `gcTime` in v5 |
| `useTable(` | `@tanstack/react-table` v7; v8 is `useReactTable` |
| `accessor:` | v7 column shape; v8 uses `accessorKey` / `accessorFn` |

Every one of those is a **substring**. That is the whole trick: API
currency is objectively checkable, so it does not need a judge and must
not be left to one.

## How other harnesses steer

Worth copying, from reading their repos:

**opencode — vendor the library, forbid recall.** Its `effect` skill
(`.opencode/skills/effect/SKILL.md`) is blunt about it:

> Use the current Effect v4 / effect-smol source, **not memory or older
> Effect v2/v3 examples**. If `.opencode/references/effect-smol` is
> missing, clone it there. **Search** it for exact APIs, examples, tests,
> and naming patterns **before answering**.

The skill does not describe the API — it points at the source and makes
consulting it a step. That is the direct antidote to stale training data,
and it generalises to any fast-moving dependency.

**opencode — AGENTS.md as enforced house style.** Not aspiration:
specific, checkable rules ("avoid `any`", "prefer functional array
methods", "inline a value used once", with good/bad code pairs), plus
branch-name and conventional-commit formats. Concrete enough that a
reviewer — or a scenario — can tell whether it was followed.

**claw-code-agent — "match existing style, even if you'd do it
differently."** One line, and it is the rule that keeps an agent from
modernising code nobody asked it to touch.

**What we already have.** `CLAUDE.md` says to use the Context7 MCP server
for up-to-date library docs; `smooth-tools` ships `create_skill`; skills
live in `.claude/skills/` and `~/.smooth/plugins/`. The gap versus
opencode is that nothing *requires* a source/doc lookup before writing
against a fast-moving library — Context7 is available, not mandatory.

## How we measure it

`crates/smooth-bench/frontend-scenarios.toml`, run with:

```bash
smooth-bench agentic --scenarios crates/smooth-bench/frontend-scenarios.toml \
  --model deepseek-v4-flash --model gpt-5.5 --scoreboard board.json
```

These use `kind = "hybrid"`, which scores **both** halves and exists
because they ask different questions:

- **Objective** — `requires` / `forbids` scan the whole workspace for
  substrings, each with a `reason` that is shown on failure. Not
  per-file: you do not know which file a stale idiom will land in, and
  naming one lets the same mistake pass in another. `node_modules` is
  skipped — it is scaffolding we handed the agent, not its work.
- **Rubric** — an LLM judge on design quality, loading/empty states,
  whether sorting actually sorts. No crisp ground truth; needs a model.

A hybrid fails fast on the objective half and **does not spend a judge
call** on work that already used a dead API — and reports the fact, which
is actionable, rather than a rubric opinion about it.

> [!warning] These score the code, not a build
> The agentic sandbox is default-deny egress, so `npm install` cannot
> run. A real `tsc` / `next build` gate needs a pre-baked toolchain in
> the image — tracked in th-f39abc.

## Keeping it honest

The table above **will** rot; that is its nature. When a scenario starts
failing every model, check whether the ecosystem moved before assuming
the models got worse — the leaderboard's "no model passed these" callout
is exactly that signal. Refresh the pins against `apps/web`'s
`package.json`, which is the version of "modern" we are actually paid to
match.

## Related

- [[Bench-Harness]] — the suites, the leaderboard, the scoreboard badge
- [[LLM-Request-Parameters]] — why a model can score 0% for a reason that isn't quality
