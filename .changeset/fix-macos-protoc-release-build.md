---
"@smooai/smooth": patch
---

release: install `protoc` on macOS runners in the binary-build matrix

The Release workflow's cross-platform binary-build matrix installed
`protobuf-compiler` only on Linux (`if: runner.os == 'Linux'`), so the
`aarch64-apple-darwin` / `x86_64-apple-darwin` targets had no `protoc` and
`smooai-smooth-narc`'s prost `build.rs` failed with "Could not find protoc".
This job had never run to completion before (the Version Update step always
failed first on the changeset package-name bug, th-645e54), so the gap was
never exposed. Added a macOS-gated `brew install protobuf` step. Pearl
th-14bddf.
