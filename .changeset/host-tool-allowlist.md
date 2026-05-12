---
'@smooai/smooth': patch
---

Make internal/Tailscale hostnames reachable from the sandbox via
`host_tool`:

- Add `host_tool` and `tool_hints` to the runner's default policy
  allowlist. `host_tool` is conditionally registered (only when
  `SMOOTH_HOST_TOKEN` is set during sandbox dispatch); listing it in
  the default `[tools].allow` lets the agent actually call it. Wonk
  still gates the underlying CLI choice on the host side via the
  separate host-tools allowlist (`gh`, `git`, `kubectl`, `jq`,
  `curl`).
- Add a `check if a host is reachable` tool hint pointing at
  `host_tool({tool: "curl", …})` with the right `-fsS -o /dev/null
  -w '%{http_code}'` template, plus an explicit note that there is
  no `http_fetch` tool — anyone reaching for one is hallucinating.
- Add a "Hostnames, 'ping', and 'is X up?'" section to the fixer
  prompt telling the agent to take bare hostnames literally (no
  `.com` guessing), explaining why `bash ping` fails inside the
  sandbox (no Tailscale, no ICMP), and pointing at host_tool as the
  canonical path for internal hosts.

Combined, "can you ping smoo-hub" now reaches `host_tool({tool:
"curl", args: ["http://smoo-hub"]})` instead of either denied
`http_fetch` calls or `bash ping smoo-hub.com` chasing the wrong
TLD.
