fn main() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    cc::Build::new()
        .file(repo_root.join("third_party/sqlite_vec/sqlite-vec.c"))
        .include(repo_root.join("third_party/sqlite_vec"))
        .include(repo_root.join("third_party/sqlite"))
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-variable")
        .compile("sqlite_vec");
    println!(
        "cargo:rerun-if-changed={}",
        repo_root.join("third_party/sqlite_vec/sqlite-vec.c").display()
    );
}
