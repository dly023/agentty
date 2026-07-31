// Stamp every binary with the source commit so a fixed 0.0.1 version policy
// does not make distinct builds indistinguishable (the GUI uses this to tell
// "the running daemon predates my binary" apart from "same build").
fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir("../..")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "dev".to_string());
    println!("cargo:rustc-env=AGENTTY_BUILD_SHA={sha}");
}
