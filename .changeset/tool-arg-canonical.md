---
"smooai-smooth-operator": patch
"smooai-smooth-operator-runner": patch
---

Tool-call wire format is now strictly canonical: `function.arguments` always serializes to a JSON-object string, never `"null"` or a primitive. Strict providers (qwen3-coder-plus on DashScope) reject anything else with `InternalError.Algo.InvalidParameter: The "function.arguments" parameter of the code model must be in JSON format.` Fix lives in `canonical_tool_arguments_json`; also replaces the streaming-parse fallback from `Value::Null` to `Value::Object(empty)` so malformed deltas don't poison the next-turn echo. New `smooth_operator::quirks` module is the home for future per-upstream-model tweaks — seeded with qwen3 / qwen-coder flags, otherwise empty.
