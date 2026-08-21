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

| stale idiom          | why it is wrong now                                      |
| -------------------- | -------------------------------------------------------- |
| `tailwind.config.js` | Tailwind 4 is CSS-first — tokens live in `@theme`        |
| `@tailwind base;`    | v3's three directives; v4 is one `@import "tailwindcss"` |
| `getServerSideProps` | pages-router data fetching, removed in the App Router    |
| `forwardRef` wrapper | React 19 passes `ref` as an ordinary prop                |
| `useQuery(key, fn)`  | TanStack Query v5 takes a single options object          |
| `cacheTime`          | renamed `gcTime` in v5                                   |
| `useTable(`          | `@tanstack/react-table` v7; v8 is `useReactTable`        |
| `accessor:`          | v7 column shape; v8 uses `accessorKey` / `accessorFn`    |

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
opencode is that nothing _requires_ a source/doc lookup before writing
against a fast-moving library — Context7 is available, not mandatory.

## Greenfield is the harder case — and the baseline is bad

The frontend suite seeds a `package.json`, so a model can read the stack
off disk and "match the existing project". **Greenfield has nothing to
read**, which turns the usual advice into "use whatever you remember".

Measured baseline, `deepseek-v4-flash`, empty workspace, unguided
(2026-08-07):

> _"Build me a small web dashboard that shows a table of sales deals with
> the total value at the top."_

It produced `index.html`, `styles.css`, `app.js`, `data.js` and a
hand-rolled `server.js`. **No React, no framework at all.** Not a stale
React stack — no stack. It works, and it is unusable as the start of
anything in our codebase.

That is the case for steering in one result. `crates/smooth-bench/greenfield-scenarios.toml`
scores it, and `.claude/skills/greenfield-stack/SKILL.md` is the
intervention: the house stack, the current-API traps, and a mandatory
verification step (Context7 MCP when there is network, vendored source
when there is not).

Run the suite with and without the skill available. That difference is
the only honest measure of whether steering works — and it is why the
suite tests **both** directions: `greenfield-dashboard` checks that the
house default gets applied, and `greenfield-respects-named-stack` checks
that an explicit user instruction still beats it. A suite that only
tested the first would teach the agent to override people.

### The A/B: steering works, measured

Same prompt, same model (`deepseek-v4-flash`), same empty workspace. The
only difference is `.smooth/skills/greenfield-stack/SKILL.md` seeded into
the workspace, which `smooth-cast`'s project-scoped discovery picks up.

|           | unguided                               | steered                                                 |
| --------- | -------------------------------------- | ------------------------------------------------------- |
| scaffold  | `index.html` + hand-rolled `server.js` | `create-next-app`                                       |
| framework | **none** — vanilla JS                  | next **16.3.0**, react **19.2.8**                       |
| styling   | hand-written CSS                       | tailwind **4** + `@tailwindcss/postcss`, no config file |
| table     | hand-rolled `<table>`                  | `@tanstack/react-table` **8.21.3**                      |
| routing   | —                                      | App Router (`src/app/`), incl. `loading.tsx`            |

Note the versions: the steered run produced next **16.3**, _newer_ than
the 16.2 pinned in the skill's own table. It scaffolded with
`create-next-app` rather than hand-writing config, so it inherited
current defaults for free — which is exactly what the skill's build order
tells it to do, and the reason that step is written as "use the
framework's own tool, not by hand".

That is the argument for a golden path over a version table: the table
rots, the procedure does not.

> [!warning] Two caveats on this run
> It was run against a **host** daemon, which has network — so
> `create-next-app` and `npm install` could actually run. Inside the
> agentic sandbox (default-deny egress) they cannot, which is why the
> sandboxed suites score code rather than scaffolding.
>
> The convo bench originally recorded this trial `INCONCLUSIVE` ("the
> assistant never said anything") — it discarded any turn with no prose,
> even one that had called a dozen tools. Fixed (th-b59d2b): a silent
> turn is only ungradeable when it ALSO called no tools. The same run now
> scores **PASS 5/5/5/5**, with the judge citing the stack it chose.

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
