---
"@smooai/smooth": minor
---

Major session: Changesets CI, web dashboard, provider overhaul, port forwarding, security enforcement.

- Changesets versioning with auto Cargo.toml sync and GitHub Actions release pipeline
- Husky git hooks with pre-commit cargo checks
- th up daemon mode (background process with pid file), th down kills it
- Interactive th auth login with provider/model picker and connection test
- Kimi + Kimi Code providers (OpenAI-compat and Anthropic-compat)
- Removed deprecated OpenCode Zen module entirely
- Web dashboard: shadcn UI components, Tailwind v4 dark theme, responsive sidebar
- Multi-project pearl support: /api/projects, project switcher, per-project pearl views
- Pearl kanban with search, timeline view, stats view with bar charts
- System topology SVG graph (radial layout, pulsing nodes, auto-refresh)
- Clickable dashboard cards navigating to system/pearls
- Port forwarding Phase 1: PortPolicy, forward_port tool, Wonk /check/port
- VM path mapping: guest→host translation for filesystem deny pattern enforcement
- Rich ANSI colors across CLI (th up/down/status/auth/doctor)
- Doctor checks on th code startup
