---
'@smooai/smooth': patch
---

gpt-6-luna is the default model (th-3030cd). It landed on llm.smoo.ai on 2026-09-22 at under half gpt-5.6-luna's price ($0.11/$0.57 vs $0.23/$1.38 per M tokens) and passes the tool-calling and temperature-0 probe. The `smooth-coding`/`smooth-default` aliases, `th operator serve`, and the web model picker now default to it, and gpt-6-astra joins the premium tier. The picker always lists the default model, even before its first bench run: such a row reads "not yet benched" instead of carrying an invented score or cost. A saved choice of the previous default (gpt-5.6-luna) follows the new default once; re-picking gpt-5.6-luna after that sticks.
