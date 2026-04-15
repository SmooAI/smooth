---
"@smooai/smooth": patch
---

Diagnostic logging on the sandboxed dispatch path so we can tell
*why* `th run` / `th code --headless` fail when they do:

- `bill::exec_sandbox` logs exec start + non-zero-exit with
  captured stdout/stderr tails (was silent before, making code=-1
  failures opaque).
- Dispatch handler now runs a preflight `/bin/sh` probe against
  the sandbox before exec-ing the runner — surfaces whether
  bind-mounts landed, whether the runner binary is visible + executable,
  and what the guest's `/opt` actually contains.

Pearl `th-1ec3ce` (P0) tracks the underlying issue: on plain alpine,
microsandbox's bind-mounts aren't reaching the guest at all, so
every sandboxed dispatch fails with `exit=-1 stderr=""`. Fix requires
digging into microsandbox's mount-arg plumbing; these changes just
give us the visibility to do it.
