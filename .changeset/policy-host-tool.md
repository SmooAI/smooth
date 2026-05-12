---
'@smooai/smooth': patch
---

Add `host_tool` and `tool_hints` to the bigsmooth policy generator's
`registered_tool_names()`. The previous fix only touched the runner's
fallback `default_policy_toml()`, but Big Smooth's dispatch generates
the actual policy that Wonk enforces — and that list was missing
both tools. `host_tool({tool:"curl",args:["http://smoo-hub"]})` was
still being denied with `host_tool is not in the tool allowlist`
despite the runner having registered it.

Sync test updated to pin the new entries so the two lists can't
drift again.
