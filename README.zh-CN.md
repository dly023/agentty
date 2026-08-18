<div align="center">

<img src="assets/app-icon.svg" alt="agentty" width="88" height="88" />

# Agentty

[English](./README.md)

**面向本地与 SSH 环境的 terminal-native AI 工作区。**

</div>

[![CI](https://github.com/dly023/agentty/actions/workflows/ci.yml/badge.svg)](https://github.com/dly023/agentty/actions/workflows/ci.yml)
[![Version](https://img.shields.io/github/v/tag/dly023/agentty?label=version&color=3FDD8C)](https://github.com/dly023/agentty/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-%E5%8A%A0%E5%85%A5%E7%BE%A4%E7%BB%84-5865F2?logo=discord&logoColor=white)](https://discord.gg/s3dethqz2V)

**Agentty 适合已经在真实终端里跑 AI coding 的开发者。**
它不替代 Claude Code、Codex、Opencode、Pi 等 CLI Agent，而是把终端工作区补齐为可恢复的现场：显式环境、可发现会话、持久化项目上下文和可恢复状态。

## 为什么是 terminal-native

Agent 工作不是单纯对话，而是持续运行的工作现场：

- PTY 与 shell
- 环境变量和工作目录
- 本地/远程文件根目录
- 主机凭据与远程能力
- 可重启、可恢复的进程

多数工具想在终端之外去管理这些状态。Agentty 的目标是让真实终端和 shell 里的 agent 保持为事实源，再在上层补上恢复与编排能力。

## Agentty 做了什么

- **Agent 优先的会话** — pane 里真实运行的 agent 进程会被发现和索引。支持的情况下可恢复会话，保留 cwd、环境和会话 id。
- **持久环境** — 本地、SSH、WSL 都是第一类环境上下文；切换环境时，终端、会话列表、项目根和文件视图联动切换。
- **远端辅助程序投放** — 远端 SSH 工作区优先使用版本匹配的 `agentty-server`，通过既有连接就地分发与启动。
- **会话稳定性** — 重启后可继续，支持会话 id 复制、持续运行窗口上下文，必要时可按底层 agent 能力 fork 分支。
- **终端内工作台** — 工作区树、文件浏览、代码增强输入、会话导航都在同一个终端应用里完成。

## 一次典型流程

1. 启动 Agentty，由本地和远端发现 agent pane。
2. 在会话导航器中选中目标会话并恢复，回到对应工作目录和项目。
3. 在本地 / SSH / WSL 环境间切换，环境状态彼此分离却同步可见。
4. 继续长任务，通过命令历史和会话分支能力在同一上下文内推进。

## 远程 helper 交付流程

远端主机不需要直接访问 GitHub 即可运行：

1. Agentty 探测目标主机 OS 与架构。
2. 通过版本号匹配对应的 `agentty-server` 压缩包（例如 `agentty-<version>-<os>-<arch>.tar.gz`）。
3. 通过现有 SSH 连接上传安装（优先 `rsync`，回退 `scp`）。
4. 远端通过同一 artifact 启动服务，保证协议兼容。

源码构建/调试场景会上传本地编译出的同一 artifact，避免本地与远端协议不一致。

## 包含内容

| | |
|---|---|
| **输入** | 历史影子补全 · 可解释 Tab 补全 · 语法高亮 · 多行编辑 · 点击移动光标 · <kbd>⌃ R</kbd> 模糊历史 |
| **窗口** | 标签与分屏 · <kbd>⌘ P</kbd> 命令面板 · <kbd>⌘ F</kbd> 滚动日志搜索 · 主题 · 输入法 |
| **Agents** | 支持的 CLI Agent 状态指示、通知、分支/差异上下文、会话恢复与 fork 支持 |
| **SSH / 远端** | 原生 russh 栈、Keychain 凭据档案、SFTP 面板、端口转发、跳板机、WSL 工作区 |

详细特性见 [docs/features.zh-CN.md](docs/features.zh-CN.md)，<kbd>⌘ ,</kbd> 打开设置可查看并重绑快捷键（含完整清单）。

## 安装

三平台原生构建都在 [**Releases**](https://github.com/dly023/agentty/releases)：

| | | |
|---|---|---|
| **macOS** | `…-macos-arm64.dmg` · `…-x86_64.dmg` | 拖入 Applications |
| **Windows** | `…-setup.exe` · 便携版 `….zip` | |
| **Linux** | `…-x86_64.AppImage` | `chmod +x` 直接运行，x11/wayland 依赖已打包 |

## 基准测试

同一台机器、同一天、统一 155×40 网格 —— Apple M1 Pro，macOS 26.3.1，五次运行均值（2026-07-04）：

| | **agentty** | Alacritty | Ghostty | Kitty |
|---|---:|---:|---:|---:|
| 纯文本 IO — 11 MB `cat` <sub>（越低越好）</sub> | **95 ms** | 239 ms | 179 ms | 185 ms |
| [DOOM-fire](https://github.com/const-void/DOOM-fire-zig) 帧率 <sub>（越高越好）</sub> | **888 fps** | 485 fps | 552 fps | 617 fps |
| 冷启动内存 | 116 MB¹ | 105 MB | 128 MB | 130 MB |

<sub>¹ GUI 105 MB + 常驻守护进程 11 MB。</sub>

复现和方法：[`scripts/bench/`](scripts/bench/README.md)。

## Agentty 不是

- 不是云端 IDE。
- 不是包装/篡改会话的黑盒。
- 不是替代任何现有 CLI Agent 的替身。

## 状态与预期

Agentty 的远程连接、会话恢复和跨环境行为还在持续演进。界面细节和本地化也在不断完善。
欢迎提 bug、提文档修正、写测试、开 PR。

### 源码运行

```bash
MACOSX_DEPLOYMENT_TARGET=10.14 cargo build --bin agentty-app
TERM=xterm-256color MACOSX_DEPLOYMENT_TARGET=10.14 ./script/run
```

更多本地开发说明见 [DEVELOPMENT.md](DEVELOPMENT.md)。

### 文档

- [文档索引](docs/README.md)
- [功能清单](docs/features.zh-CN.md)
- [更新日志](CHANGELOG.md)

### 与上游关系

Agentty 基于开源终端与编辑器基础持续演进，保留了核心渲染、输入和会话运行框架，并持续清理与云依赖相关的路径。
主要底层来自：

- [warp](https://github.com/warpdotdev/warp)
- [zap](https://github.com/zerx-lab/zap)

## 许可

[Apache-2.0](LICENSE)

---

<div align="center">
<sub>
基于 [gpui](https://github.com/zed-industries/zed) 与 [`alacritty_terminal`](https://github.com/zed-industries/alacritty) 构建 · [Apache-2.0](LICENSE) · [Discord](https://discord.gg/s3dethqz2V) · [更新日志](CHANGELOG.md)
</sub>
</div>
