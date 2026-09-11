use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::Path;

// Source closure, not artifact/toolchain identity. Keep the build script's
// rerun roots and the hashing roots identical, including directories for adds.
pub const INPUTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "crates/tty7-core/Cargo.toml",
    "crates/tty7-core/build.rs",
    "crates/tty7-core/build_support",
    "crates/tty7-core/src",
    "crates/tty7-server/Cargo.toml",
    "crates/tty7-server/src",
];

fn collect(root: &Path, name: &str, files: &mut Vec<String>) -> io::Result<()> {
    let path = root.join(name);
    let kind = fs::symlink_metadata(&path)?.file_type();
    if kind.is_file() {
        files.push(name.to_owned());
    } else if kind.is_dir() {
        for entry in fs::read_dir(path)? {
            let leaf = entry?.file_name();
            let leaf = leaf
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF8 source path"))?;
            if leaf.contains(['\\', '\n', '\r']) {
                return Err(io::Error::other("nonportable source path"));
            }
            collect(root, &format!("{name}/{leaf}"), files)?;
        }
    } else {
        return Err(io::Error::other(format!(
            "not a regular source file/directory: {name}"
        )));
    }
    Ok(())
}

pub fn fingerprint(root: &Path) -> io::Result<String> {
    let mut files = Vec::new();
    for name in INPUTS {
        collect(root, name, &mut files)?;
    }
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"tty7-source-identity-v1\0");
    for name in files {
        let bytes = fs::read(root.join(&name))?;
        // Length framing prevents path/content boundary ambiguities, including
        // embedded NUL bytes in source assets. No lossy path normalization.
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
