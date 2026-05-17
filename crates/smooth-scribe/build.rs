//! Compiles `proto/scribe.proto` from the workspace root via tonic-build.
//! Pearl th-893801 iter-2.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("proto");
    let scribe_proto = proto_root.join("scribe.proto");

    println!("cargo:rerun-if-changed={}", scribe_proto.display());

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[scribe_proto], &[proto_root])?;

    Ok(())
}
