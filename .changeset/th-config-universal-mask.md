---
"smooai-smooth": patch
---

th config: mask all echoed values (last-4 disclosure) regardless of tier

`th config set` previously echoed public-tier values raw and secret-tier
values masked to the last 4 characters. `th config list` echoed
everything raw with no tier-awareness at all — same class of footgun
as raw `pnpm sst secret list` (CLAUDE.md §13, SMOODEV-908).

Tier no longer affects the echo. Both `set` and `list` mask every
value to its last 4 characters. Public-tier keys can still be
sensitive (CDN tokens, allowlist entries, anything an attacker could
correlate) and the UX cost of `***wert` over `password-qwert` is
trivial vs the cost of training users that console echo is a safe
confirmation surface.

`th config get` is unchanged — it's an explicit retrieval, not a
side-effect echo, and reveal-on-demand is the right contract there.
A future `--reveal` flag for explicit unmasking on `set` / `list`
remains open as a follow-up if the UX hurts.

Pearls th-4ebbf7 + th-9cc412.
