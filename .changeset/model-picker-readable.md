---
'@smooai/smooth': patch
---

Big Smooth's model picker is readable again (SMOODEV-4174). Each model's name gets its own line instead of being squeezed to "g." by the score and cost text, and the full id sits underneath. The main models (GPT-6 Luna, then Claude Haiku 5.5) are listed first. 🏆 now needs at least two benched models and a leader at 60% or better, so a lone 17.9% model no longer wears it. Premium is a text tag rather than a 💎. Unbenched rows say "not benched" once, an unmeasured $/pass is noted once in the footer, and the unexplained heart glyphs are replaced by the token price as text. The bench catalog now carries its run date, which the footer shows with its age, and the daemon serves Haiku's bench score from the full leaderboard.
