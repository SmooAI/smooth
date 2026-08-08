---
'@smooai/smooth': minor
---

Plugins are a daemon surface (th-262e5f). `plugin.toml` manifests have been a
config format with nothing reading them since the microVM operative was deleted
— the loader went with it, so an installed plugin was a file on disk the agent
never saw. The daemon now owns them: `smooth_tools::plugin` merges
`~/.smooth/plugins/<name>/plugin.toml` with the workspace's
`.smooth/plugins/` (project shadows global) and registers each enabled manifest
on the operator's **per-turn** registry, so a plugin sits behind the permission
gate and Narc like a built-in and its command runs through the same kernel OS
sandbox `bash` does — not a raw subprocess. Discovery runs per turn, so
`th plugin init` takes effect on the next message instead of the next restart.

`GET /api/plugins` serves the merged catalog (manifest fields + scope +
registered tool name + path, plus any manifests that failed to parse), honoring
the same guarded `?cwd=` override as `/api/skills`, so a face with no disk
access can render the list the agent actually has rather than re-walking the
directories itself. Tool names are now `plugin_<name>` rather than
`plugin.<name>` — a `.` is invalid in a provider tool name and would have
failed the whole LLM request.
