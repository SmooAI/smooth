---
'@smooai/smooth': patch
---

`th` CLI rough-edge sweep — exhaustive dispatch, readable help, scriptable output.

`main`'s command dispatch ended in a `Some(_) => "Command not yet implemented.
Coming soon!"` arm that returned `Ok`. That wildcard defeated match
exhaustiveness, so an unwired command exited 0 instead of failing the build —
which is exactly what `th project` had been doing. The arm is gone: `th project
list` now shares the working `th pearls projects` implementation, and `th
project create` exits non-zero pointing at `th pearls init`.

`th -h` was a ~300-line prose wall. Without a blank `///` line clap folds an
entire doc comment into short help, and most of the top-level `Commands`
variants lacked one — the worst summary ran 452 characters. Every multi-sentence
variant (and the four top-level options) now breaks after its first sentence;
nothing was deleted, the detail moved to `th <cmd> --help`. A test asserts every
subcommand's short help stays one sentence.

`--json` landed on the read commands that had no machine-readable form: `agents
list`, `files ls`, `testing {deployments,cases,environments,runs} list`,
`referrals show`, `referrals link`, and `referrals partners list`. Everything
else in those modules already emitted raw JSON. `print_orgs_list` was a near-copy
of `print_list_envelope` differing only in showing a `slug` where the shared
helper showed a `status`; the helper now falls back to `slug` and the copy is
gone.

`th doctor` gained a reclaimable-disk section — `~/.smooth/build-*-target` dirs
with sizes, the stale pre-SMOODEV-1739 `~/.smooth/auth/` tree, an unrotated
`service.log`, and `providers.json.bak*` sprawl. It reports and prints the exact
`rm`; it never deletes.

Also: `Registry::load` treats a zero-byte `~/.smooth/registry.json` as an empty
registry instead of erroring, which had made every `auto_register` a silent
no-op; the vulnerable `atty` dep (RUSTSEC-2021-0145) is replaced by
`std::io::IsTerminal`; and the docs no longer claim 54 top-level commands or
document the non-existent `th pearls migrate-from-sqlite`.
