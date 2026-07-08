---
"@smooai/smooth": patch
---

fix(daemon): give Big Smooth chat assistant-grade token/iteration headroom

The daemon's chat path resolved config via `local_config()` →
`ServerConfig::from_env()`, silently inheriting the customer-support **chat
widget** defaults (`512` max_tokens / `6` max_iterations). A reasoning model
like `deepseek-v4-flash` spends its entire 512-token budget on
`reasoning_content` and gets cut off before emitting any `content` — the
"Big Smooth seems hung" empty reply — while 6 iterations makes any
read-a-few-files turn hit "Maximum iterations reached". `resolve_gateway_config`
now gives the daemon `32768` tokens / `50` iterations unless
`SMOOTH_AGENT_MAX_TOKENS` / `SMOOTH_AGENT_MAX_ITERATIONS` explicitly override.

Also guards `BigSmoothFace` WebGL init so a GPU-unavailable browser degrades
to no 3D face instead of crashing the whole SPA.
