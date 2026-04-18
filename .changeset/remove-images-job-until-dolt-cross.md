---
"@smooai/smooth": patch
---

Remove the `images` job from the release workflow until we fix
smooth-dolt's aarch64-linux-musl cross-compile.

Current state:

- `ghcr.io/smooai/smooth-operator:0.2.0` / `:latest` and
  `ghcr.io/smooai/boardroom:0.2.0` / `:latest` are already public on
  GHCR (pushed manually the day we went public). Smooth pulls `:latest`
  by default so end users are unaffected.
- The `images` job was green through docker login after the `GH_PAT`
  scope fix, but then failed on `build-boardroom.sh` — that script
  expects a cross-compiled `smooth-dolt` at
  `target/aarch64-unknown-linux-musl/release/smooth-dolt`, which
  nothing currently produces. `build-smooth-dolt.sh` is a host-arch
  `go build` that lands at `target/release/` (glibc-linked), so the
  alpine-based boardroom image can't copy it.

Options for the follow-up (tracked in a pearl):

1. Switch the boardroom image base from alpine to
   `debian:slim-aarch64` so a host-linked smooth-dolt runs natively.
2. Cross-compile smooth-dolt to aarch64-musl using `zig cc` as the Go
   CGO compiler (the same zigbuild workflow Rust uses).
3. Build smooth-dolt inside a containerized alpine stage during
   `docker build` and COPY the result.

Until then, image pushes are manual via
`scripts/build-smooth-operator-image.sh --push` and
`scripts/build-boardroom-image.sh --push`.
