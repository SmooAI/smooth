---
'@smooai/smooth': minor
---

Big Smooth sandbox: personal-assistant posture. The macOS Seatbelt profile
confined writes to the workspace, so the assistant could not edit `~/.zshrc`,
`~/.config`, or any dotfile — the things you would actually ask a personal
agent to do. Writes are now permitted by default, with kernel-level denies
retained only where they must hold unconditionally: credential stores
(`~/.ssh`, `~/.aws`, gh/gcloud/kube/docker/gnupg, `.netrc`, `~/.smooth/auth`,
`providers.json`), `~/Library/LaunchAgents`, and `.git/hooks` / `.git/config`
in every repository rather than just the workspace.

Intent-level safety moves to the behavioural layers, which can distinguish a
benign config edit from a harmful one where a path-based rule cannot: the
engine DenyPolicy circuit-breakers and the Narc LLM judge.
