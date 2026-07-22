---
'@smooai/smooth': patch
---

Add `th operator serve --lang <rust|go|ts|python|dotnet> [--port <n>]` — a
first-class subcommand that boots any of the 5 polyglot smooth-operator
LocalServer implementations behind a uniform env contract, promoting the
`scripts/operator-serve.sh` dogfooding launcher into `th` (discoverable help,
per-engine caveats, `SMOOTH_OPERATOR_REPO` path resolution). Ships a
`switch-operator-engine` Big Smooth skill so Big Smooth can switch engines
itself. Pearl th-3f46fd.
