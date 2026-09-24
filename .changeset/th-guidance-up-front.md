---
'@smooai/smooth': patch
---

Big Smooth knows how to use `th` before it starts (th-cf34d9). The `th` tool's description pointed at old spellings (`api <resource>`, the removed `api whoami`) and said nothing about the CRM, so a question like "what are my biggest deals" cost a turn of `--help` probing. The description now leads with verified recipes for the `smoo` namespace (pipeline, deals, contacts, tasks, reminders, invoices, analytics, campaigns, account, pearls, agent mail), tells the model to run `<area> ai` once for a full guide instead of walking `--help`, to use `--json` when computing, and to hand a sign-in error to the user rather than retry. The persona gains a short "user's business" section, and a test parses every concrete example in the description against the real CLI so the recipes can't go stale.
