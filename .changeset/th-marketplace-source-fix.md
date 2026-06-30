---
"@smooai/smooth": patch
---

fix(smooth marketplace): smooth-agent plugin failed to install ("source type your Claude Code version does not support")

The marketplace used `metadata.pluginRoot` + a bare `"source": "smooth-agent"`. On Claude Code 2.1.196 that combination is rejected as an unsupported source type. Switched to the canonical explicit relative-path form `"./claude-plugins/smooth-agent"` (matching the official marketplace's working `"./plugins/<name>"` entries) and dropped `pluginRoot`. No change to the plugin itself.
