---
"@smooai/smooth": patch
---

smooth-dolt: forward push/pull args into CALL DOLT_*() (pearl th-9eb6a0)

`smooth-dolt push <dir>` previously called `CALL DOLT_PUSH()` with no
args, silently dropping any trailing `-u origin <branch>` / `-f` flags
that the Rust CLI appends for first-push auto-retry. First push to a
fresh remote (smooblue today, any new project tomorrow) returned
`fatal: The current branch main has no upstream branch.` and stayed
errored even though the Rust matcher detected the case and called
`push --set-upstream origin main`.

Fix: parse `os.Args[3:]` and bind each as a positional SQL arg to
`CALL DOLT_PUSH(?, ?, ?)` / `CALL DOLT_PULL(?, ?, ?)`. Zero-arg
callers stay on the no-parens form so behavior is unchanged for them.
