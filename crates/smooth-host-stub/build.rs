//! Compiles `proto/host_stub.proto` via tonic-build.
//!
//! Pearl th-893801 Phase 2 iter-4a.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("proto");
    let proto_file = proto_root.join("host_stub.proto");
    println!("cargo:rerun-if-changed={}", proto_file.display());

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto_file], &[proto_root])?;
    Ok(())
}
