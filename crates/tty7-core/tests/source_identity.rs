#[path = "../build_support/source_identity.rs"]
mod source_identity;

use source_identity::{INPUTS, fingerprint};
use std::fs;
use std::path::Path;

fn fixture(root: &Path) {
    for name in INPUTS {
        let path = root.join(name);
        if name.ends_with("/src") || name.ends_with("/build_support") {
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("fixture.rs"), "same source\n").unwrap();
        } else {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "same manifest\n").unwrap();
        }
    }
}

#[test]
fn source_identity_matches_independent_sha256_vector_and_compiled_stamp() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    // Independently calculated with Ruby Digest::SHA256 and Q< length framing.
    assert_eq!(
        fingerprint(root.path()).unwrap(),
        "5a82be48940127ea3c9b9df812e0d9bd4a83d0ba72d629ef6e6f4cb5d1c19de5"
    );
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    assert_eq!(fingerprint(workspace).unwrap(), env!("TTY7_SOURCE_SHA256"));
}

#[cfg(unix)]
#[test]
fn source_identity_rejects_special_files() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let socket = root.path().join("crates/tty7-core/src/socket");
    let _listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    assert!(fingerprint(root.path()).is_err());
}

#[test]
fn source_identity_is_location_and_creation_order_independent() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    fixture(a.path());
    fixture(b.path());
    for (root, names) in [(a.path(), ["a.rs", "z.rs"]), (b.path(), ["z.rs", "a.rs"])] {
        for name in names {
            fs::write(root.join("crates/tty7-core/src").join(name), name).unwrap();
        }
    }
    let before = fingerprint(a.path()).unwrap();
    assert_eq!(before, fingerprint(b.path()).unwrap());
    fs::create_dir(a.path().join("target")).unwrap();
    fs::write(a.path().join("target/generated"), "artifact").unwrap();
    fs::create_dir(a.path().join("src")).unwrap();
    fs::write(a.path().join("src/gui.rs"), "GUI only").unwrap();
    assert_eq!(before, fingerprint(a.path()).unwrap());
    assert_eq!(before.len(), 64);
}

#[test]
fn source_identity_tracks_content_names_addition_removal_and_lockfile() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let baseline = fingerprint(root.path()).unwrap();
    for name in INPUTS {
        let path = root.path().join(name);
        let file = if path.is_dir() {
            path.join("fixture.rs")
        } else {
            path
        };
        let original = fs::read(&file).unwrap();
        fs::write(&file, "changed").unwrap();
        assert_ne!(baseline, fingerprint(root.path()).unwrap(), "{name}");
        fs::write(&file, original).unwrap();
        assert_eq!(baseline, fingerprint(root.path()).unwrap());
    }
    let added = root.path().join("crates/tty7-server/src/new.data");
    fs::write(&added, "included non-Rust asset").unwrap();
    let with_added = fingerprint(root.path()).unwrap();
    assert_ne!(baseline, with_added);
    let renamed = added.with_extension("renamed");
    fs::rename(&added, &renamed).unwrap();
    assert_ne!(with_added, fingerprint(root.path()).unwrap());
    fs::remove_file(renamed).unwrap();
    assert_eq!(baseline, fingerprint(root.path()).unwrap());
}

#[test]
fn source_identity_missing_input_is_an_error() {
    for name in INPUTS {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        let path = root.path().join(name);
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        assert!(fingerprint(root.path()).is_err(), "{name}");
    }
}

#[cfg(unix)]
#[test]
fn source_identity_does_not_follow_symlink_files_or_directories() {
    for target in ["Cargo.lock", "crates/tty7-core/src"] {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path());
        std::os::unix::fs::symlink(
            root.path().join(target),
            root.path().join("crates/tty7-server/src/link"),
        )
        .unwrap();
        assert!(fingerprint(root.path()).is_err());
    }
}
