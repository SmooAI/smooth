# Provider notes (Qwen / Alibaba family)

You are running on a Qwen-class model. The base prompt's discipline rules are the playbook — there are no provider-specific overrides. Two recurring failure modes for your class on this codebase to watch:

- **English-only output in code, comments, and chat unless the user is writing in another language.** This codebase is English; matching it keeps diffs reviewable.
- **Native tool-call schema, not pseudo-code blocks.** If you're about to wrap a tool invocation in ``` ```tool_code ``` ``` fences, stop and emit a real tool call.
