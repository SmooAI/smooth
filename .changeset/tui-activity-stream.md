---
"@smooai/smooth": patch
---

th smooth TUI: surface coding-workflow activity inline

Today the TUI forwards `TokenDelta`, tool calls, and the final
`Completed`/`Error` events. Everything else the runner emits
(iteration boundaries, snapshots, max-iter caps, budget breaches,
Warn-level Narc alerts) was silently dropped. So a long workflow
run looked like one streaming blob with no signal of what was
actually happening.

The 7-phase decomposition (ASSESS / PLAN / EXECUTE / VERIFY /
REVIEW / FINALIZE) is gone — see
`crates/smooth-operator/src/coding_workflow.rs:15`. Only the
single `CODING` phase + an iteration counter remain. So the
"phase breadcrumbs" idea collapses into "iteration breadcrumbs".

`handle_agent_event` now surfaces:

- `PhaseStart { iteration, alias }` → inline system line
  `→ iteration N • {alias}`. Lands once per outer iteration of
  the coding workflow so the user can see the workflow pacing.
- `CheckpointSaved { iteration }` → muted line `✓ snapshot taken
  (iter N)`. Confirms the best-seen-workspace snapshotting is
  doing its job.
- `MaxIterationsReached { max }` → `⚠ hit max iterations (N) —
  stopping`. Was previously dropped on the floor with no user-
  facing signal.
- `BudgetExceeded { spent_usd, limit_usd }` → `⚠ budget exceeded
  — spent $X of $Y`. Same — was silent.

`ServerEvent::NarcAlert` handling is now severity-aware:

- `Block` (the call was actually denied) → unchanged, surfaces
  as `Error` and terminates the run.
- `Warn` (informational alert, did NOT block execution) → new
  inline system message `⚠ Narc Warn • {category}: {msg}`. The
  run keeps going; the user sees the warning. Previously every
  Warn was incorrectly routed as an Error and killed the
  response.
- Anything else → quiet.

The `category` field of NarcAlert is now plumbed through (was
dropped via `..`) so the user knows whether the alert is
about secrets, prompt injection, etc.
