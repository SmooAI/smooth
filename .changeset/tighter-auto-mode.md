---
'@smooai/smooth': patch
---

SECURITY (th-85d481): tighter auto-mode decide() engine. Command/process substitution (`$(…)`, backticks, `<(…)`) contents are now evaluated as their own policy segments, so `echo $(env)` is denied instead of riding in on `echo`; `~/.smooth/providers.json`, `~/.smooth/auth/` and dotenv files (`.env`, `.envrc`, `.env.*`) join the sensitive-path deny list (token-scoped so `rg "process.env"` stays allowed); `find` loses safe-bin status under `-exec`/`-execdir`/`-ok`/`-delete`/`-fprint*`; `git config` is no longer auto-allowed and `git branch`/`git remote` are restricted to listing forms; read-category tools now hit the same credential-path circuit-breaker as bash and writes.
