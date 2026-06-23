---
'smooai-smooth-tools': patch
---

Phase 3 hardening (th-08e05a / EPIC th-c89c2a): extend the macOS Seatbelt
bash sandbox to cover more of the plan's P1 acceptance criteria. Writes to
`workspace/.git/config` are now kernel-denied alongside `.git/hooks` (a
writable config can repoint `core.hooksPath` or add executing aliases —
P1 #5). Credential read-denial grows beyond `~/.ssh`/`~/.aws`/`~/.config/gh`/
`~/.gnupg` to also cover `~/.config/gcloud`, `~/.kube`, `~/.docker`, and
`~/.netrc` (P1 #6). Adds adversarial tests proving a planted hook /
`.git/config` write fails and cloud/registry creds don't leak.
