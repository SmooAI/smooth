---
'@smooai/smooth': minor
---

Claude Haiku 5.5 and GPT-6 Luna are now both main models in Big Smooth and `th code`, and you can switch between them per turn (SMOODEV-4148). Haiku is not on the Smoo gateway. When you have an Anthropic key in `providers.json`, the daemon's new loopback model router sends Haiku turns to Anthropic and everything else to the gateway. The pickers offer Haiku only when it can run. A `th code` `/model` pick of the main slot now switches the live session immediately and is used again the next time you start `th code`.
