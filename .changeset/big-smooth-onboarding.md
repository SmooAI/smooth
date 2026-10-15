---
'@smooai/smooth': minor
---

Big Smooth now helps a new user pick a model on first run. The choice is the Family plan ($9/month, 14-day free trial, hosted models through the Smoo AI gateway) or your own OpenAI-compatible or Anthropic key. Your own key is tested with one request before it is saved.

The picker appears in these places:

- `th up`, when no model is configured
- `th model setup`
- `th doctor --onboard`
- a first-run card in the web UI

In chat, Big Smooth can switch endpoints with the new `configure_provider` tool, which asks you to approve each change. Big Smooth also reports when a switch needs a restart.
