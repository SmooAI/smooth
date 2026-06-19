---
"@smooai/smooth": patch
---

build: make `smooth-cast` track the workspace version (`version.workspace = true`)

`crates/smooth-cast/Cargo.toml` hardcoded `version = "0.13.7"` while every
other workspace crate uses `version.workspace = true`. When the changeset
Version PR bumped the workspace to `0.14.0`, all siblings followed but
`smooth-cast` stayed `0.13.7`, so `cargo build --examples --workspace` failed
with "failed to select a version for `smooai-smooth-cast = ^0.14.0` … candidate
0.13.7 … required by smooai-smooth-bench v0.14.0", blocking the version PR's
Rust checks (and thus the publish). Only exposed once `changeset version`
finally ran end-to-end. Pearl th-d050a3.
