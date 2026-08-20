use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    validate_i18n_catalogs("assets/i18n");
    ensure_daemon_sibling_built();
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/favicon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/favicon.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=failed to embed Windows icon: {e}");
        }
    }
}

/// Compile-time fail-closed for I18N-EXHAUSTIVE-TRANSLATION-06: every shipped
/// locale catalog must share one key set and every value must be non-empty.
fn validate_i18n_catalogs(dir: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    println!("cargo:rerun-if-changed={dir}");

    let mut paths: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{dir}: {e}"))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            path.extension()
                .is_some_and(|ext| ext == "yaml")
                .then_some(path)
        })
        .collect();
    paths.sort();

    if paths.len() < 2 {
        panic!("{dir}: expected at least two locale catalogs (*.yaml)");
    }

    let mut key_sets: Vec<(PathBuf, HashSet<String>)> = Vec::new();
    for path in &paths {
        println!("cargo:rerun-if-changed={}", path.display());
        let keys = parse_catalog_keys(path);
        key_sets.push((path.clone(), keys));
    }

    let (base_path, base_keys) = &key_sets[0];
    for (path, keys) in &key_sets[1..] {
        let only_base: Vec<_> = base_keys.difference(keys).cloned().collect();
        let only_other: Vec<_> = keys.difference(base_keys).cloned().collect();
        if !only_base.is_empty() || !only_other.is_empty() {
            let mut msg = format!(
                "i18n catalog key set divergence: {} vs {}",
                base_path.display(),
                path.display()
            );
            for key in only_base {
                msg.push_str(&format!("\n  only in {}: {key}", base_path.display()));
            }
            for key in only_other {
                msg.push_str(&format!("\n  only in {}: {key}", path.display()));
            }
            panic!("{msg}");
        }
    }

    eprintln!(
        "i18n catalogs ok: {} ({} keys)",
        key_sets
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy())
            .collect::<Vec<_>>()
            .join(", "),
        base_keys.len()
    );
}

fn parse_catalog_keys(path: &Path) -> HashSet<String> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut seen = HashMap::<String, usize>::new();
    let mut keys = HashSet::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            panic!("{}:{}: missing ':' separator", path.display(), lineno + 1);
        };
        let key = key.trim().to_string();
        let mut value = value.trim();
        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            value = &value[1..value.len() - 1];
        }
        if key.is_empty() {
            panic!("{}:{}: empty key", path.display(), lineno + 1);
        }
        if let Some(first) = seen.insert(key.clone(), lineno + 1) {
            panic!(
                "{}:{}: duplicate key '{key}' (first at line {first})",
                path.display(),
                lineno + 1
            );
        }
        if value.trim().is_empty() {
            panic!("{}:{}: empty value for '{key}'", path.display(), lineno + 1);
        }
        keys.insert(key);
    }
    keys
}

/// Dev/prod GUI builds must place `agentty-server` beside `agentty-app` in the
/// active target profile (LOCAL-RUNTIME-EXECUTABLE-02). `cargo run --bin agentty-app`
/// only compiles the GUI crate; seed the sibling from an existing release build or
/// warn to run `cargo build -p agentty-server --locked`.
fn ensure_daemon_sibling_built() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let target_dir = PathBuf::from(
        std::env::var("CARGO_TARGET_DIR")
            .unwrap_or_else(|_| manifest_dir.join("target").display().to_string()),
    );
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let server_name = if cfg!(windows) {
        "agentty-server.exe"
    } else {
        "agentty-server"
    };
    let server_path = target_dir.join(&profile).join(server_name);

    println!("cargo:rerun-if-changed=crates/agentty-server/src");
    println!("cargo:rerun-if-changed=crates/agentty-server/Cargo.toml");
    println!(
        "cargo:rerun-if-changed={}",
        target_dir.join("release").join(server_name).display()
    );

    if server_path.is_file() {
        return;
    }

    let release_path = target_dir.join("release").join(server_name);
    if release_path.is_file() {
        if let Some(parent) = server_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::copy(&release_path, &server_path).unwrap_or_else(|e| {
            panic!(
                "failed to copy {} -> {}: {e}",
                release_path.display(),
                server_path.display()
            );
        });
        eprintln!(
            "seeded sibling {} from {}",
            server_path.display(),
            release_path.display()
        );
        return;
    }

    println!(
        "cargo:warning=agentty-server is missing beside agentty-app; \
         run `cargo build -p agentty-server --locked` (or `cargo app`) before opening terminals"
    );
}
