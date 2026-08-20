# Agentty Early Development Rules

Agentty is in an early architecture phase. Correctness and conceptual clarity take precedence over compatibility with unreasonable internal designs.

## 1. Debt policy

- Do not preserve an unsuitable abstraction merely to minimize the diff.
- Prefer replacing a confused model with one clear canonical model while the project is young.
- Delete superseded paths, migration-only wrappers, duplicate state, and dead compatibility layers once the new path is proven.
- Do not add a second source of truth as a temporary shortcut.
- For a managed remote Environment, the target Host/daemon is the sole authority for target-owned state (machine trees, pane facts and titles, provider stores, session user state, hooks, and remote files). Local GUI files and mirrors may contain only explicitly named client state, cache, or pending intent. Hydration, reconnect, refresh, and source failure must never promote that local data into remote facts or use it as a fake remote fallback. Any local-to-remote write goes through the one canonical HostOps/ControlRequest path with identity and ownership preconditions; unknown, stale, or rejected results fail closed.
- Compatibility is required for user data and external protocols only when a spec explicitly says so; internal Rust APIs are not stable yet.
- Every intentional temporary compromise needs an owner, removal condition, and tracker item. Untracked TODO debt is forbidden.

## 2. Product model

- The primary GUI is a single application window that switches Environment authority in place through `select_environment`; CLI and headless connect flows may still open or focus a dedicated Environment window via `open_or_focus_environment`.
- Environment is execution authority: Local, SSH, WSL, or another explicit host backend.
- One selected Environment may contain many repositories, tabs, panes, and Agent sessions; this replaces the old one-workspace-per-window product model.
- Tabs and panes inherit the selected Environment and must not silently mix execution authorities.
- The left sidebar carries an Environment rail plus the Agent Session Navigator for the selected Environment; non-current Environments may show stashed session counts without becoming content-column authority.
- The top-right Environment Indicator always shows the selected authority and switches Environments in the same window (or opens a window when no invoking app is available).
- Running `ssh` inside a terminal is an OpenSSH child process and does not change Agentty authority; choosing SSH in the indicator uses managed russh and switches Environment authority.
- Agent sessions, Composer, filesystem operations, command completion, and process launch derive authority from the selected Environment, never from incidental application focus.
- Remote authority boundary: a remote machine's persisted tree, provider evidence, session aliases/pins/order, hooks, and target-side files remain remote-owned. A local projection or EnvironmentNavigatorCache stash is never an implicit migration source during hydrate/reconnect; missing remote evidence stays missing until an explicit typed event or identity-scoped seed is accepted by the remote authority.

## 3. Required workflow

Architecture changes follow:

`SPEC → YAML TRACKER → MATRIX → static CHECK → failing TEST → IMPLEMENTATION → VERIFY → DELETE OLD PATH`

Read `AGENTS.md`, the relevant `docs/specs/*.yaml`, and `docs/quality/development-tracker.yaml` before editing architecture.

## 4. Version policy

- During this development line the repository package version is `0.0.1`.
- Push validation fails unless `Cargo.toml` and the root entries in `Cargo.lock` report `0.0.1`.
- Do not automatically increment per commit. Change this policy only through an explicit release/version spec update.

## 5. Upstream tracking policy

- Track `upstream/main` continuously, but do not merge it wholesale into Agentty.
- Review upstream commits locally with `git show`, focused diffs, and the relevant tests before selecting anything.
- Absorb only evidence-backed fixes and designs that fit Agentty's Environment, Host, and canonical runtime primitives.
- Prefer a focused `git cherry-pick` for an isolated compatible fix. For architecture or UI designs, extract behavior, failure semantics, and tests, then implement them natively through the SPEC workflow instead of copying upstream structure.
- Reject or defer changes that create large rename conflicts, import excluded distribution/update/proxy behavior, or lack a focused acceptance boundary.
- Before publishing, run the mapped focused tests, `./script/presubmit quick`, and the applicable full gate. Keep the final Agentty delta as one squashed commit relative to the selected `origin/main` base.
- Never use the latest `upstream/main` as an implicit release base. If upstream has advanced materially, record the selected commit IDs and continue from the existing Agentty base.
