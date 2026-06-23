---
'smooai-smooth-daemon': patch
---

Phase 3 hardening (EPIC th-c89c2a): broaden the permission engine's
remote-code-execution circuit-breakers. Previously only `curl|sh` /
`wget|bash` were caught; now a downloader piped into *any* interpreter
(`python`/`python3`/`perl`/`ruby`/`node`/`zsh`/`dash`/`ksh`, plus `|&`
variants) trips the breaker, as does a command-substituted download fed to
`eval` or an interpreter `-c` (`eval "$(curl …)"`, `bash -c "$(wget …)"`).
Detection is segment-based on `|` so an interpreter name appearing as a
substring (`shellcheck`) is not a false positive. Circuit-breakers fire in
every mode including `auto`/`bypass`, where they are the last backstop
before the kernel sandbox. Adds adversarial + lookalike tests.
