//! Compiles `proto/wonk.proto` from the workspace root via tonic-build.
//!
//! wonk.proto imports `smooth/narc/v1/narc.proto` for the Scope enum,
//! so the include root must be the workspace `proto/` directory.
//!
//! Pearl th-893801 iter-2.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("proto");
    let wonk_proto = proto_root.join("wonk.proto");
    let narc_proto = proto_root.join("narc.proto");

    println!("cargo:rerun-if-changed={}", wonk_proto.display());
    println!("cargo:rerun-if-changed={}", narc_proto.display());

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        // narc.proto's types live in the smooth-narc crate already.
        // Route wonk's reference to `smooth.narc.v1.Scope` at the
        // existing generated module instead of duplicating it.
        .extern_path(".smooth.narc.v1", "::smooth_narc::pb")
        .compile_protos(&[wonk_proto], &[proto_root])?;

    Ok(())
}
