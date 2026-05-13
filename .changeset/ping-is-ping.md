---
'@smooai/smooth': patch
---

"ping" means `ping`, not curl-as-a-stand-in:

- Add `ping`, `dig`, `nslookup`, `host` to the host_tool CLI
  allowlist (`crates/smooth-bigsmooth/src/host_tools.rs`). All are
  reconnaissance-only, no host-state mutation.
- Tool hints reorganized: separate `intent = "ping a host"` (ICMP)
  from `intent = "check if a host is reachable on a port"` (HTTP),
  plus a new `resolve a hostname` hint for `dig`/`host`.
- Fixer prompt explicit: don't conflate "curl failed on port 80"
  with "host down" — many hosts answer ICMP but don't run HTTP.
  If the user asked to "ping", actually run `ping`.
