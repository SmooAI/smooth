---
"@smooai/smooth": minor
---

Big Smooth becomes a conversational team lead, and operators can talk back. Plan: `~/.claude/plans/sorted-orbiting-hummingbird.md`.

- **Big Smooth chat is now agentic.** `POST /api/chat` runs an `Agent` loop with six tools: `pearls_search`, `pearls_show`, `pearls_create` (auto-titled via smooth-summarize), `teammate_spawn` (with `working_dir` + `role`), `teammate_message`, `teammate_read`. Default model is the reasoning slot (smooth-reasoning-kimi); `model` field on `ChatBody` overrides per-request. System prompt is goal-first, bias-toward-action.
- **Pearl-comment mailbox.** Operators read steering / direct-chat / answers via a 1.5 s comment poll, injected into the agent loop as user-turns. New `AgentConfig.chat_rx`. Prefix routing: `[CHAT:USER]`, `[STEERING:GUIDANCE]`, `[ANSWER:USER|SMOOTH:q-id]`.
- **Operator-side `ask_smooth` and `reply_to_chat` tools.** Blocking and fyi modes. Shared `QuestionRegistry` resolves blocking calls when the matching `[ANSWER:*:q-id]` lands.
- **Teammate registry + REST.** `AppState.teammates: OperatorRegistry`. `GET /api/teammates`, `GET/POST /api/teammates/{name}/messages`, `POST /api/teammates/{name}/shutdown`. Per-pearl `comment-tap` broadcasts `TeammateChat` / `TeammateSpawned` / `TeammateIdle` events.
- **Bench through Big Smooth.** `smooth-bench` now POSTs `/api/chat` and polls the pearl until `[IDLE]` or quiescence, instead of calling `run_headless_capture` directly. `SMOOTH_BENCH_LEGACY_DIRECT=1` falls back.
- **Env plumbing.** `SMOOTH_PEARL_ID` reaches every operator. `SMOOTH_WORKFLOW_MAX_ITERATIONS` and `SMOOTH_WORKFLOW_AGENT_MAX_ITERATIONS` flow through both dispatch paths and the inner agent loop.

Web UI sidebar (Shift+ArrowDown cycle, Lead pinned + Teammates section) and SSE streaming + per-session chat budget are planned follow-ups (Phase 4 UI half + Phase 6).
