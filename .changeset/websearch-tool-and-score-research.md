---
"smooai-smooth": minor
---

WebSearch tool (Exa MCP primary, Parallel fallback) + Wonk allowlist + score-research bench dimension

**New tool — `web_search`** (pearl th-2cc3f1). Mirrors OpenCode's
`tool/websearch.ts` + `tool/mcp-websearch.ts`: posts an MCP JSON-RPC
`tools/call` to a hosted LLM-tuned search provider that returns
extracted, LLM-ready text (no separate fetch step needed for the
snippets). Two providers behind the same surface so smooth-vs-opencode
head-to-head benches stay on the same backend:

- **Exa** (`mcp.exa.ai`) — primary. Tool name `web_search_exa`, knobs
  `type` / `numResults` / `livecrawl` / `contextMaxCharacters`.
- **Parallel** (`search.parallel.ai`) — fallback. Tool name `web_search`.

Provider picked from `SMOOTH_EXA_API_KEY` / `SMOOTH_PARALLEL_API_KEY`;
`SMOOTH_WEBSEARCH_PROVIDER=exa|parallel` overrides. Registers only when
a provider key is configured — otherwise the tool would always error
on first call and just clutter the schema.

**Wonk policy** (pearl th-bf3f6e). Adds `mcp.exa.ai` + `search.parallel.ai`
to `phase_network_defaults()` baseline. Single-purpose, easy to audit,
no wildcards.

**`score-research` bench dimension** (pearl th-f4ac64). Sibling to
`score-cleanup`. Grades the agent's ability to answer questions that
REQUIRE web search — fact lookups, identifying a title from a fuzzy
description, etc. Two axes: `answer_correctness` (case-insensitive
keyword matching, `min_correctness` default 1.0 hard-kills below
threshold) and `cited_source` (URL detection — anti-hallucination
probe). First fixture `research-hijack-year` probes the chain
end-to-end (find "Hijack" series → year + service → cite).

Reuses `CoachCfg` + `AgentDriver` trait so mock/opencode/smooth/pi
all work out of the box. Mock agent `perfect-research-hijack.sh`
makes the pipeline runnable in CI without API spend.
