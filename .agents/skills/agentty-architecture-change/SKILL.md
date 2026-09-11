---
name: agentty-architecture-change
description: Use when changing Agentty architecture, SSH behavior, agent sessions, persistence, local/remote authority, i18n, or process/materialization entrypoints.
user-invocable: true
---

# Agentty Architecture Change

1. Read `AGENTS.md`, the domain spec, and `docs/quality/traceability.yaml`.
2. Update or add a stable contract ID before implementation.
3. Add the matrix row: source, static check, test, verification, evidence.
4. Write a failing focused test or a deterministic reproduction.
5. Implement through the canonical path; do not add a parallel side-effect path.
6. Run the focused test and `./script/presubmit quick`.
7. For release-impacting changes run `./script/presubmit full` and record evidence.
