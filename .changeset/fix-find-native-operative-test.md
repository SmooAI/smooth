---
'smooai-smooth-bigsmooth': patch
---

Fix `find_native_operative_finds_debug_or_release_build` to accept the
`cargo install`ed binary location. `find_native_operative_binary()`
deliberately falls back to `$CARGO_HOME/bin` / `~/.cargo/bin` (th-92dac3)
when a `target/<profile>/` build isn't found near the manifest — which is
exactly the case in a worktree using a shared target dir. The test
asserted the path must be under `target/release|debug`, so it failed in
that (valid) setup. The assertion now also accepts a `bin/` install dir
while keeping the guard that the path must not be the
`aarch64-unknown-linux-musl` cross-compile output.
