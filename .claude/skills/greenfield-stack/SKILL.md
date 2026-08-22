---
name: greenfield-stack
description: The house stack and the from-nothing build procedure — what to reach for when there is no existing project to match, and how to verify versions are current before writing. Use whenever starting something new ("build me a dashboard", "make a landing page", "new app", "from scratch", "scaffold"), or when picking a library for a greenfield feature.
---

# Building from nothing

Most guidance says "match the existing code". Greenfield has no existing
code — so that rule silently becomes "use whatever you remember", and
what you remember is your training data. That is the failure this skill
exists to prevent.

The two rules, in order:

1. **Do not invent a stack.** Use the house stack below.
2. **Do not trust the versions below, or your memory of the APIs.**
   Verify before writing (§ Verify).

## The house stack

Pinned to what `smooai/apps/web` actually ships, because "modern" means
"survives our codebase", not "recent".

| Need          | Use                                                   | Not                                                |
| ------------- | ----------------------------------------------------- | -------------------------------------------------- |
| App framework | **Next 16**, App Router, Server Components by default | `create-react-app`, Vite-for-an-app, pages router  |
| UI runtime    | **React 19**                                          | class components, `React.FC`                       |
| Styling       | **Tailwind 4**, CSS-first `@theme` tokens             | `tailwind.config.js`, CSS-in-JS, styled-components |
| Components    | **shadcn/ui** primitives (Radix under it)             | a component library nobody asked for               |
| Server state  | **TanStack Query v5**                                 | `useEffect` + `fetch` + three `useState`s          |
| Tables        | **TanStack Table v8**                                 | hand-rolled `<table>` with sort state              |
| Forms         | **react-hook-form** + zod resolver                    | uncontrolled `<form>` + manual validation          |
| Icons         | **lucide-react**                                      | an icon font                                       |
| Dates         | `Intl` / `date-fns`                                   | moment                                             |

**Server Components are the default.** Reach for `"use client"` only when
the component needs state, effects, or event handlers — and put it as low
in the tree as possible, not at the page root.

## Current-API traps

The idioms below are what a model trained a year or two ago produces.
They compile, they read fine, and they are wrong for this stack:

| Wrote                | Should be                                   |
| -------------------- | ------------------------------------------- |
| `tailwind.config.js` | `@theme { --color-brand: … }` in CSS        |
| `@tailwind base;`    | one `@import "tailwindcss";`                |
| `getServerSideProps` | a Server Component, or `fetch` in the route |
| `forwardRef(...)`    | `ref` as an ordinary prop (React 19)        |
| `useQuery(key, fn)`  | `useQuery({ queryKey, queryFn })`           |
| `cacheTime`          | `gcTime`                                    |
| `useTable(...)` (v7) | `useReactTable` + `getCoreRowModel()`       |
| `accessor:`          | `accessorKey` / `accessorFn`                |
| `useFormState`       | `useActionState`                            |

## Verify before you write

**This step is not optional, and it is the whole point of the skill.**
The table above has a date on it and the ecosystem does not care.

1. **Context7 MCP** — resolve the library, read the _current_ docs for
   the exact API you are about to use. Prefer this; it is live.
   Configure it once in `~/.smooth/mcp.toml`:

    ```toml
    [[servers]]
    name = "context7"
    command = "npx"
    args = ["-y", "@upstash/context7-mcp"]
    disabled = false
    ```

2. **No Context7 / no network?** Then read the source. Vendor the library
   into `.smooth/references/<lib>` and search it for the real API,
   examples and naming — the way opencode's `effect` skill does. Source
   beats memory; memory is the thing that is wrong.

3. **Still unsure of a version?** `npm view <pkg> version`. Never write a
   version number from memory into a `package.json`.

If you cannot verify, say so in your reply rather than guessing
confidently. A stated uncertainty is cheap; a confidently stale API costs
a debugging session.

## The build order

1. **Scaffold** with the framework's own tool (`npx create-next-app@latest`),
   not by hand-writing config files. It gets current defaults for free.
2. **Verify** the versions it produced are what you expected, and read
   what it generated before adding to it.
3. **Build the smallest working thing**, then iterate. A dashboard with
   one real table beats five stubbed panels.
4. **Handle loading and empty states** — the first render has no data.
   This is the most common gap in generated UI.
5. **Say what you chose and why** in one or two lines. A stack decision
   the user cannot see is one they cannot correct.

## When the user names a different stack

They win. Use what they asked for, and mention the house default once if
it is materially better for what they described — then drop it. Do not
relitigate.

## Related

- `docs/Engineering/Benchmarking-Modern-Stacks.md` — how we measure whether this works
- `crates/smooth-bench/greenfield-scenarios.toml` — the scenarios that score it
