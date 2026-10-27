---
'@smooai/smooth': patch
---

SMOODEV-4148: Claude Haiku 5.5 now runs through the `llm.smoo.ai` gateway like every other model, and the in-daemon direct Anthropic router is removed. Haiku is in the published model catalog for the web picker and the phone apps, and `th code` offers it on the gateway provider. A `providers.json` that still has the Coding slot on the Anthropic provider for Haiku is sent to the gateway.
