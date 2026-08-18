use std::fs;
use std::path::{Path, PathBuf};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x100_0000_01b3;

fn collect_rust_sources(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rust_sources(&path, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Exact, deterministic identity of the server source closure linked into both
/// the desktop server and cross-compiled helpers. The commit alone is
/// insufficient for local packaging: two dirty builds can share HEAD while
/// speaking different wire/provider vocabularies or starting through different
/// server entrypoints.
fn source_tree_fingerprint(manifest_dir: &Path) -> std::io::Result<String> {
    let server_dir = manifest_dir
        .parent()
        .expect("agentty-core remains under the workspace crates directory")
        .join("agentty-server");
    let mut files = vec![
        manifest_dir.join("Cargo.toml"),
        manifest_dir.join("build.rs"),
        server_dir.join("Cargo.toml"),
    ];
    collect_rust_sources(&manifest_dir.join("src"), &mut files)?;
    collect_rust_sources(&server_dir.join("src"), &mut files)?;
    files.sort();

    let mut hash = FNV_OFFSET;
    for path in files {
        let relative = path
            .strip_prefix(manifest_dir.parent().unwrap_or(manifest_dir))
            .unwrap_or(&path);
        let portable = relative.to_string_lossy().replace('\\', "/");
        hash = hash_bytes(hash, portable.as_bytes());
        hash = hash_bytes(hash, &[0]);
        hash = hash_bytes(hash, &fs::read(&path)?);
        hash = hash_bytes(hash, &[0xff]);
    }
    Ok(format!("{hash:016x}"))
}

// Stamp every binary with both its source commit and exact core tree. This
// keeps the fixed 0.0.1 version policy while making clean and dirty builds
// distinguishable at handshake/package time.
fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../agentty-server/Cargo.toml");
    println!("cargo:rerun-if-changed=../agentty-server/src");

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
    );
    let repository = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("agentty-core remains in <repo>/crates/agentty-core");
    let revision = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repository)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "dev".to_string());
    let fingerprint = source_tree_fingerprint(&manifest_dir)
        .expect("agentty-core source tree must be readable to stamp a build");
    println!("cargo:rustc-env=AGENTTY_BUILD_SHA={revision}.{fingerprint}");
}
