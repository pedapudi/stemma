//! Regenerates the checked-in Rust code for proto/ into crates/stemma-proto/src/gen.
//!
//! This is a stopgap for wiring the rules_rust prost toolchain into Bazel: the
//! protos in proto/ stay canonical, and this tool (run via tools/regen_protos.sh)
//! refreshes the generated code whenever they change.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("proto-gen lives at <repo>/tools/proto-gen")
        .to_path_buf();

    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);

    let out_dir = repo_root.join("crates/stemma-proto/src/gen");
    std::fs::create_dir_all(&out_dir)?;

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir(&out_dir)
        .compile_protos(
            &[
                repo_root.join("proto/stemma/v1/resolve.proto"),
                repo_root.join("proto/stemma/v1/embedder.proto"),
            ],
            &[repo_root.join("proto")],
        )?;

    println!("generated into {}", out_dir.display());
    Ok(())
}
