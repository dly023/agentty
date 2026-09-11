---
name: agentty-rust-check
description: Use when running Rust checks, focused tests, cargo check, formatting, or diagnosing a long Agentty workspace build.
user-invocable: true
---

# Agentty Rust Check

- Start with the smallest package and test filter that proves the changed behavior.
- Use `--locked`; do not silently rewrite `Cargo.lock`.
- Run one Cargo process at a time to avoid build-lock contention.
- Minimum sequence: `cargo fmt --check`, focused test, `./script/presubmit quick`.
- Use `./script/presubmit full` before delivery when shared core/protocol/persistence behavior changes.
- If dependency fetch or compilation is long, report the active phase instead of launching duplicate builds.
