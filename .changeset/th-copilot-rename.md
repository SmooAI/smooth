---
'@smooai/smooth': patch
---

Rename `th api copilot` → `th api smooth-operator`. The org's always-on dashboard
agent is now called "Smooth Operator" (echoing the OSS `smooth-operator` package).
Renames the CLI subcommand, its module (`copilot.rs` → `smooth_operator.rs`), the
`org-copilot` skill (→ `smooth-operator`), and updates the API request paths to
`/organizations/{org}/smooth-operator/*` to match the renamed backend routes.
