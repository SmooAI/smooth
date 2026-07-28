---
"smooth": patch
---

`th status` no longer reports `error decoding response body` against a healthy daemon. `/health` is served by smooth-operator's `LocalServer` and answers with the plain string `ok`, never JSON, so parsing it as JSON failed every time. Health probing now lives in one place (`daemon_health`) shared by `th status`, `th doctor`, `th model status` and the auto-start path, and it distinguishes the outcomes a user actually needs: daemon down, a foreign service squatting the port, or up — each with one line naming the state and one naming the fix. The status panel also stopped printing "healthy" for subsystems it never checked.
