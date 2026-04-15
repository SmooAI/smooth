---
"@smooai/smooth": patch
---

Wordmark + cast renames in user-facing surfaces:

- `th` CLI + web dashboard now say "Big Smooth" (not "Leader") and
  "Smooth Operators" (not "Sandbox").
- New horizontal logo (`images/logo.png`, `crates/smooth-web/web/public/logo.svg`)
  replaces the old mark.
- `th up` / `th status` / `th doctor` banners render "Smooth" with
  the logo's gradient colors via ANSI 24-bit truecolor escapes
  ("Smoo" orange→pink, "th" teal→blue).
- `/health` service field renamed `smooth-leader` → `big-smooth`.
- `SMOOTH_SANDBOX_MAX_CONCURRENCY` env + `th up --max-operators N`
  flag expose the previously hardcoded pool cap of 3.
