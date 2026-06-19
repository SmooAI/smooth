---
"@smooai/smooth": patch
---

Wire `AgentConfig::with_verify_tests_before_done` into the operator-runner dispatch path (pearl th-393aed). The builder landed earlier today but no caller was using it, so the "no final response until tests pass" rule wasn't actually firing in any bench run. Now: `smooth-operator-runner` reads `SMOOTH_VERIFY_TESTS` from its env and calls the builder with the parsed boolean (`1` / `true` → on, anything else / unset → off). Big Smooth's per-task operator-runner spawn (`server.rs`'s minimal env_clear + whitelist) now also passes the var through, alongside the other `SMOOTH_WORKFLOW_*` knobs. Default off — general `th code` sessions still see no behavior change; bench runs flip it on by booting Big Smooth with `SMOOTH_VERIFY_TESTS=1 th up direct …` so all in-bench operator runs see the rule.
