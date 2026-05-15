//! Emit `TH_VERSION` at compile time so Big Smooth's startup log
//! can print "v0.8.0 (abc123)". Duplicates `crates/smooth-cli/build.rs`
//! to keep the value available without a shared helper crate —
//! a few lines of duplication is cheaper than a new workspace member.

use std::process::Command;

fn main() {
    let pkg_version = env!("CARGO_PKG_VERSION");
    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty());

    let version_string = match git_sha {
        Some(sha) => format!("{pkg_version} ({sha})"),
        None => pkg_version.to_string(),
    };

    println!("cargo:rustc-env=TH_VERSION={version_string}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");

    // Compile proto/bigsmooth.proto via tonic-build. bigsmooth.proto
    // imports narc.proto — route those types through smooth-narc's
    // `pb` module so we don't duplicate enums. Pearl th-893801.
    if let Err(e) = compile_protos() {
        // Don't fail the build for proto issues during this iter —
        // log loud but keep building. The grpc module is gated on
        // successful codegen by being a separate src file that
        // tonic::include_proto!s the output.
        println!("cargo:warning=tonic-build for bigsmooth.proto failed: {e}");
    }
}

fn compile_protos() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("proto");
    let bs_proto = proto_root.join("bigsmooth.proto");
    let narc_proto = proto_root.join("narc.proto");

    println!("cargo:rerun-if-changed={}", bs_proto.display());
    println!("cargo:rerun-if-changed={}", narc_proto.display());

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .extern_path(".smooth.narc.v1", "::smooth_narc::pb")
        .compile_protos(&[bs_proto], &[proto_root])?;
    Ok(())
}
