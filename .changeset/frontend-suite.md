---
'smooth': patch
---

bench: frontend build-and-judge suite — modern-stack API currency (Next 16 / React 19 / Tailwind 4 / TanStack v5)

The well-known coding benchmarks are almost entirely algorithmic Python and measure
nothing about building UI on a stack that shipped this year. The differentiating
failure there is stale idioms from older training data — `tailwind.config.js`,
`forwardRef` wrappers, positional `useQuery(key, fn)`, `cacheTime`, v7 `useTable`.
The code compiles, reads well, and a judge calls it good.

Adds `kind = "hybrid"` (objective assertions AND a rubric — API currency is fact,
design quality is not) and workspace-wide `requires`/`forbids` pattern rules that
each carry a `reason` shown on failure. Four scenarios pinned to what `apps/web`
actually ships. A hybrid fails fast on the objective half and doesn't spend a judge
call on work that already used a dead API.
