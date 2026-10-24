---
'@smooai/smooth': patch
---

Big Smooth no longer tells the model "you are already signed in" when no Smoo session exists. The persona and the `th` tool description used to assert that unconditionally, which contradicted the signed-out account section. Signed out, the agent now works from data in the workspace that fits the request instead of stopping at "run `smoo auth login`".
