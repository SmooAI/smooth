---
'@smooai/smooth': patch
---

smooth-bench + `operator-serve.sh`: always rebuild the TypeScript LocalServer instead of reusing whatever is in `dist/` (pearl th-11284c).

Both launchers skipped `pnpm install && pnpm build` whenever `typescript/server/dist/main.js` merely existed. `dist/` is gitignored, so it can be arbitrarily older than the checkout — and on a machine where it predated the coding toolset (th-82ad57), the bench booted a chat-only bundle: the server started, turns completed and produced text, but ZERO tools were ever registered. That scored as a model-quality FAIL (ts 0/2, empty tools column) rather than the stale-artifact problem it was, while rust/go/python passed with tools. A stale `node_modules` bites identically (the engine dep had moved 0.1.1 → 1.7.1 without a reinstall), so the install runs too.

`tsc` is incremental (~7s cold, ~2s warm), which is what Go's unconditional `go build` and Python's `uv sync` in the same function already cost. With the rebuild in place, `agentic --engine ts --model deepseek-v4-flash` passes with `read_file,edit_file` in the tools column.
