---
'@smooai/smooth': patch
---

Bump smooai-smooth-operator-core 1.7.0 → 1.7.2 so streaming turns read the
gateway's `x-litellm-response-cost` header (smooth-operator-core#102). Every
real agent turn streams, so cost_usd was always $0 in the daemon spend ledger,
`th code` status bar, and the bench leaderboard. Pinned precisely at 1.7.2:
1.7.3+ adds a `details` field to `AgentEvent::ToolCallComplete` that the
git-pinned smooth-operator crate (rev c893323) pattern-matches exhaustively,
so newer cores break the build until that pin catches up.
