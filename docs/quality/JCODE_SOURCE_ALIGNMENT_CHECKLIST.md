# Jcode Source Alignment Checklist

基准源码：`refs/jcode`
目标：Agentty 只吸收 Jcode 已验证的会话边界、标题和 resume 语义，不复制私有实现。

## 已核对并落地

- [x] **官方会话存储目录**
  - Jcode 源码：`crates/jcode-desktop/src/session_data.rs`
  - 规则：扫描 `~/.jcode/sessions/*.json`，排除 `.journal.json`。
  - Agentty：`agent_runtime/service.rs::discover_jcode_session_files`

- [x] **主会话边界**
  - Jcode 源码：`crates/jcode-base/src/session.rs`
  - 规则：`parent_id.is_some()` 和 `is_debug` 属于内部会话，不作为用户主会话展示。
  - Agentty：Jcode 文件发现过滤 `parent_id` 和 `is_debug`。

- [x] **system-reminder 可见性**
  - Jcode 源码：`session.rs::is_internal_system_reminder_message`、`tool/session_search.rs::is_system_like_message`
  - 规则：以 `<system-reminder>` 开头的用户消息属于内部消息。
  - Agentty：Jcode 文件发现不再以这类消息作为会话存在或标题依据。

- [x] **标题优先级**
  - Jcode 源码：`session_data.rs` 的 `custom_title/title`，以及消息内容。
  - Agentty 统一规则：用户 alias > 有效 provider 会话名 > 首条真实用户消息 > 默认标题。
  - 规则已写入 `SESSION-TITLE-PRECEDENCE-11`。

- [x] **Jcode 消息内容形态**
  - Jcode 源码：StoredMessage content 支持字符串、数组和对象内容块。
  - Agentty：`jcode_message_text` 支持上述三种形态。

- [x] **resume 入口**
  - Jcode 源码：CLI 使用 `jcode --resume <session_id>`。
  - Agentty：所有 Agent 统一进入 typed `ResumeInvocation`，Jcode 只提供 CLI 语法，不在 UI 拼 shell 命令。

- [x] **resume 参数去重**
  - Agentty：统一 `CLIAgent::replay_flags` 清理旧的 `--resume/-r`，避免重复参数。
  - 已覆盖真实错误：`jcode --resume id --resume id`。

- [x] **远程环境 authority**
  - Agentty：使用选定 Host 读取远端 `~/.jcode/sessions`，不把本地 Jcode 会话伪装成远程结果。

## 仍需补强的验证项

- [x] **Jcode 文件发现 fixture 测试**
  - 覆盖：主会话、子代理、debug、journal、纯 system-reminder、system-reminder + 真实用户消息。

- [ ] **Jcode API fallback 的 system-reminder 语义**
  - 当前 stable `ListSessions` 返回字段不包含消息内容。
  - 必须确认 API 返回的 `title/status` 是否已经由 Jcode 过滤内部会话，不能在 Agentty 侧猜测。

- [ ] **Jcode 自己的显示标题规则逐字段对照**
  - 需要补齐 `short_name`、`last_active_at`、`updated_at` 和 latest user preview 的映射测试。

- [ ] **真实桌面验收**
  - 本地新终端启动 Jcode 后临时 carrier 是否出现。
  - 历史列表数量是否与 Jcode 官方列表一致。
  - 点击 resume 是否只产生一次 `--resume`。
  - 远程环境是否读取远程列表并自动展开侧栏。

## 当前结论

源码审计已完成主要路径，但“全部对齐”不能只凭静态检查宣称完成。前三项补强验证必须完成后，才能把 Jcode 会话发现标记为 verified。
