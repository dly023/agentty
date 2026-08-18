<div align="center">

<img src="assets/app-icon.svg" alt="agentty" width="88" height="88" />

# Agentty

[简体中文](./README.zh-CN.md)

**A terminal-native workspace for coding agents across local and SSH environments.**

</div>

[![CI](https://github.com/dly023/agentty/actions/workflows/ci.yml/badge.svg)](https://github.com/dly023/agentty/actions/workflows/ci.yml)
[![Version](https://img.shields.io/github/v/tag/dly023/agentty?label=version&color=3FDD8C)](https://github.com/dly023/agentty/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join%20chat-5865F2?logo=discord&logoColor=white)](https://discord.gg/s3dethqz2V)

**Agentty is for developers whose AI coding already runs in real terminals.**
It does not replace tools like Claude Code, Codex, Opencode, Pi, or AGI shell helpers.
Instead, it turns the terminal into an agent workspace: explicit environments, discoverable
sessions, persistent file/project context, and reliable recovery.

## Why terminal-native

Agent workflows are not just chat logs. They are live workstations:

- PTYs and shells
- environment variables and cwd
- local and remote file roots
- host credentials and working directories
- running processes that can die and be resumed

Most tools try to manage that work from outside the terminal. Agentty keeps the terminal as the source of truth and layers recoverability on top.

## What Agentty adds

- **Agent-first sessions** — Agent panes stay real terminal processes. Agentty discovers and indexes agent sessions, restores supported ones with their working directories and session ids, and keeps status visible in the UI.
- **Persistent environments** — Local, SSH, and WSL are treated as explicit environment contexts. Switching environments switches terminals, project roots, and file views together.
- **Remote-first delivery** — Remote SSH workspaces are driven by a matching bundled `agentty-server`, pushed over your existing connection when needed.
- **Session safety** — Resume after reboot, copy session ids, continue conversations, and use fork/branch semantics where supported by the underlying agent CLI.
- **Terminal-native IDE layer** — In-workspace project tree, file browser, rich shell input, and session navigator so you do not leave the terminal to inspect and continue work.

## A typical flow

1. Launch Agentty and let it discover local and remote agent panes.
2. Open a discovered session from the navigator and resume it to return to its cwd and project.
3. Move between local and remote environments while keeping separate but synchronized state.
4. Continue long-running work, jump by command history, and switch to a forked session path if needed.

## Remote helper workflow

Remote support does not require the remote host to reach GitHub. For a mismatched or missing remote helper:

1. Agentty detects host OS/architecture.
2. It resolves the matching `agentty-server` archive (for example `agentty-<version>-<os>-<arch>.tar.gz`) from release assets.
3. It uploads and installs the remote helper over SSH (prefer `rsync`, fallback to `scp`).
4. The remote server starts from that same artifact, keeping protocol consistency.

For local builds and source debugging, Agentty uploads the exact locally built helper artifact to the target host.

## What is inside

| | |
|---|---|
| **Input** | ghost suggestions from history · explained tab completion · syntax highlighting · multi-line editing · click to move caret · <kbd>⌃ R</kbd> fuzzy history |
| **Window** | tabs & splits · <kbd>⌘ P</kbd> palette · <kbd>⌘ F</kbd> scrollback search · themes · IME |
| **Agents** | status and notifications for supported CLIs, branch/diff context, fork and resume actions, resume-friendly tray signals |
| **SSH / Remote** | native russh stack, keychain-backed profiles, SFTP panel, port forwarding, jump hosts, WSL workspace support |

Details: [docs/features.md](docs/features.md). Open settings at <kbd>⌘ ,</kbd> to view all keybindings and remap defaults ([full list](docs/features.md#keybindings)).

## Install

Native builds are published on [**Releases**](https://github.com/dly023/agentty/releases):

| | | |
|---|---|---|
| **macOS** | `…-macos-arm64.dmg` · `…-x86_64.dmg` | drag into Applications |
| **Windows** | `…-setup.exe` · portable `….zip` | |
| **Linux** | `…-x86_64.AppImage` | `chmod +x` and run — x11/wayland deps are bundled |

## Benchmarks

Same machine, same day, same 155×40 grid — Apple M1 Pro, macOS 26.3.1,
five-run averages (2026-07-04):

| | **agentty** | Alacritty | Ghostty | Kitty |
|---|---:|---:|---:|---:|
| Plaintext IO — 11 MB `cat` <sub>(lower = better)</sub> | **95 ms** | 239 ms | 179 ms | 185 ms |
| [DOOM-fire](https://github.com/const-void/DOOM-fire-zig) frame rate <sub>(higher = better)</sub> | **888 fps** | 485 fps | 552 fps | 617 fps |
| Cold-launch memory | 116 MB¹ | 105 MB | 128 MB | 130 MB |

<sub>¹ GUI 105 MB + persistent daemon 11 MB.</sub>

Methodology and one-command reproduction: [`scripts/bench/`](scripts/bench/README.md).

## What Agentty is not

- Not a cloud IDE.
- Not a wrapper that hides or replays agent traffic through private APIs.
- Not a replacement for your existing CLI agents.

## Status and expectations

Agentty is actively evolving its remote and session workflows. Remote SSH, session recovery,
and cross-agent behavior are improving continuously, and localizations and UI polish are
still in progress.

Contributions are welcome: bug reports, docs fixes, tests, and pull requests.

### Build from source

```bash
MACOSX_DEPLOYMENT_TARGET=10.14 cargo build --bin agentty-app
TERM=xterm-256color MACOSX_DEPLOYMENT_TARGET=10.14 ./script/run
```

See [DEVELOPMENT.md](DEVELOPMENT.md) for local setup and architecture notes.

### Documentation

- [Docs index](docs/README.md)
- [Features](docs/features.md)
- [Changelog](CHANGELOG.md)

### Relationship to upstream

Agentty is rooted in open-source terminal and editor foundations and carries forward the core rendering,
input, and session runtime while removing cloud-dependent control paths where possible.
Key base layers include:

- [warp](https://github.com/warpdotdev/warp)
- [zap](https://github.com/zerx-lab/zap)

## License

[Apache-2.0](LICENSE)

---

<div align="center">
<sub>
Built on [gpui](https://github.com/zed-industries/zed) and [`alacritty_terminal`](https://github.com/zed-industries/alacritty) · [Apache-2.0](LICENSE) · [Discord](https://discord.gg/s3dethqz2V) · [Changelog](CHANGELOG.md)
</sub>
</div>
