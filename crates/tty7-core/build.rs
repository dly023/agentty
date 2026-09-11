#[path = "build_support/source_identity.rs"]
mod source_identity;

fn main() {
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets manifest directory"),
    );
    let root = manifest
        .parent()
        .and_then(std::path::Path::parent)
        .expect("core lives under workspace/crates");
    for input in source_identity::INPUTS {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }
    let fingerprint = source_identity::fingerprint(root)
        .expect("all source identity inputs must be readable regular files/directories");
    println!("cargo:rustc-env=TTY7_SOURCE_SHA256={fingerprint}");
}
