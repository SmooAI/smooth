//! Compiles `proto/narc.proto` from the workspace root via tonic-build.
//!
//! Generated module path: `smooth.narc.v1` → re-exported as
//! `smooth_narc::pb` in src/lib.rs via `tonic::include_proto!`.
//!
//! Pearl th-893801 spike (iter-1).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Workspace-root proto/ is the source of truth. All gRPC-serving
    // crates point their build.rs at the same path. The `include` arg
    // is the dir tonic searches for `import "..."` resolution; we
    // include the workspace `proto/` so future protos that import
    // (e.g. wonk imports narc.proto) resolve cleanly.
    let proto_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("proto");
    let narc_proto = proto_root.join("narc.proto");

    // Rerun build if the proto file changes.
    println!("cargo:rerun-if-changed={}", narc_proto.display());

    tonic_build::configure()
        .build_server(true)
        // Iter-1: the smooth-narc crate hosts both the proto types
        // and a thin server adapter. The bigsmooth-side SafehouseNarc
        // (which has the AccessStore + grants state) implements the
        // generated trait. We still need the client side because
        // tests in this crate spin a server and dial it.
        .build_client(true)
        .compile_protos(&[narc_proto], &[proto_root])?;

    Ok(())
}
