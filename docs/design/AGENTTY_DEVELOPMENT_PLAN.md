# Agentty 二次开发总计划

状态：Verified baseline
日期：2026-07-31

## 产品定位

Agentty 是轻量、终端优先的 CLI Agent 工作台。终端仍是实际运行面，Agentty 增加编辑、可观测、会话导航和智能补全能力，但不内置模型 BYOK 或另造聊天协议。

核心体验：

1. 左侧 Session Navigator 发现并恢复本地/远程 Agent 会话。
2. 中央终端保持完整兼容，不阻断原生 TUI 操作。
3. 底部按需弹出自由编辑 Composer，解决多行、长文本、IME、粘贴和修改困难。
4. 底部状态带展示 Agent 活动、工具调用、权限等待、耗时和上下文信息。
5. 命令补全从历史建议升级为离线、上下文感知、多源排序的智能补全。

## 当前基座判断

Agentty 已有可复用基础：

- `CmdEditor`：光标、选择、撤销/重做、kill/yank；
- `completion.rs`：命令、路径、flag/value 和 signature completion；
- `generator.rs`：git/docker/cargo 等动态 generator；
- `assets/completions/*.json`：静态命令签名库；
- shell integration：prompt、cwd、command start/end、exit code；
- Agent hook + OSC 777：Agent、事件、session ID、message、cwd；
- Agent 状态：Working / Waiting / Done、未读和通知。

结论：不替换终端内核，在现有输入模型和事件模型上增加独立 presentation layer。

## 参考项目结论

### Ashide / Zap

借鉴：统一 suggestion item（replacement、display、description、icon、match type、details）；cwd/时间/成功状态排序；signature grammar；异步 source generation；rich details。

不移植：沉重 UI/AI 模型层、云端 next-command prediction、完整 block/chat protocol。

### OpenAI Codex TUI

借鉴：Composer 独立状态机；多行输入；Enter/Shift+Enter；paste burst；draft restore；popup/footer state 分离；高度和光标位置独立计算；footer 宽度降级。

### oh-my-pi

借鉴：可组合 footer segment；按优先级和宽度折叠；hook/event tool lifecycle；recent session 与 switching 分离。

### JCode / OpenCode / DeepX

借鉴：结构化 ToolCall 生命周期；运行/完成/失败/权限等待分态；suggestion overlay 不推动终端布局；复杂详情进入抽屉。

## 能力 A：自由编辑 Composer

### 体验

默认终端行为不变。快捷键或底部 handle 打开 Composer：

```text
┌──────────────────────────────────────────────────────┐
│ 多行输入，可自由编辑、粘贴、选中、撤销……            │
├──────────────────────────────────────────────────────┤
│ Codex · agentty  Shift+Enter 换行  Cmd+Enter 发送   │
└──────────────────────────────────────────────────────┘
```

建议：

- `Cmd+J` 打开/收起；
- `Cmd+Enter` 发送；
- `Shift+Enter` 换行；
- `Esc` 关闭并保留草稿；
- 普通 Enter 默认换行，避免误提交长 prompt；
- 支持 paste、IME、选择、undo/redo；
- 草稿按稳定 pane identity 隔离；
- Agent Waiting 时可自动提示打开，但不抢焦点。

### 发送模式

- `AgentPrompt`：发送给等待输入的 CLI Agent；
- `ShellCommand`：作为 shell 命令执行；
- `RawInput`：原样写入 TTY。

首版只支持文本。附件、图片、上下文后续增加。

### 架构

```text
src/ui/composer/
  mod.rs
  model.rs
  view.rs
  keymap.rs
  delivery.rs
  draft_store.rs
```

```rust
ComposerModel {
    target: PaneIdentity,
    mode: ComposerMode,
    draft: TextBuffer,
    submission: SubmissionState,
}
```

Composer 不直接依赖临时 focus handler，统一经 `InputDelivery` canonical primitive 写入 daemon pane。

关键风险：alternate screen、bracketed paste、shell/Agent Enter 语义、pane 切换串台、远程断线 retry、IME composition。

## 能力 B：Agent Activity Bar 与 Tool Inspector

### 分层显示

底部一行摘要：

```text
● Codex 工作中 · Read src/ui/app.rs · 12s · 3 个工具调用
```

状态：空闲、思考、调用工具、等待权限、等待用户、本轮完成、工具失败。

点击后打开详情抽屉：

```text
本轮活动
  ✓ rg "session" src/                 120ms
  ✓ Read src/ui/tab_sidebar.rs          8ms
  ● cargo test -p tty7-core            18s
  ! ApplyPatch                          等待授权
```

### 数据来源优先级

1. provider 官方 hook/event；
2. Agent JSONL/session event tail；
3. tty7 已捕获的 OSC 777；
4. process tree / foreground command；
5. terminal text heuristic，仅低可信 fallback。

不得把屏幕文字解析伪装成权威 ToolCall。

### 统一事件模型

```rust
AgentActivityEvent {
    environment: EnvironmentId,
    pane: PaneIdentity,
    provider: AgentKind,
    session_id: Option<String>,
    sequence: u64,
    timestamp: SystemTime,
    kind: AgentActivityKind,
}
```

事件包含 TurnStarted、Thinking、ToolStarted/Updated/Finished、PermissionRequested、WaitingForUser、TurnFinished。

首批 adapter：Codex、Claude、generic OSC 777。原始 tool input 默认不落盘，只保存清洗摘要。

## 能力 C：智能命令补全

### 当前事实

现有能力不只是历史：已经包含 PATH command、builtin、cwd path、JSON signature、option/value/subcommand、dynamic generator 和 cwd-aware history。主要不足是 source 编排、shell 解析、排序、异步一致性和详情 UI。

### 目标模型

```rust
CompletionRequest {
    line,
    cursor,
    cwd,
    shell,
    environment,
    repo_context,
    generation,
}

CompletionCandidate {
    replacement,
    display,
    replace_range,
    kind,
    source,
    description,
    detail,
    score,
}
```

Source 优先级：

1. shell grammar/token context；
2. command signature；
3. filesystem；
4. dynamic generator；
5. cwd-aware history；
6. repo context；
7. 可选本地 predictor，后期且默认关闭。

排序考虑 prefix/fuzzy、grammar validity、cwd、成功历史、frecency、repo relevance、source confidence、失败惩罚和 stale path。重复 replacement 合并并保留最丰富 details。

### Repo-aware source

优先做无需 AI 的高价值补全：

- `package.json` scripts；
- Cargo packages/bins/features/examples；
- Makefile targets；
- justfile recipes；
- git branches/remotes/tags；
- docker compose services；
- kubectl contexts/namespaces；
- 最近成功命令的参数模式。

异步结果绑定 request generation，旧结果不得覆盖新输入。

## 与 Session Navigator 的统一关系

四项能力共享同一 EnvironmentId 和 PaneIdentity：

- Navigator materialize pane；
- Activity Bar 订阅 pane/provider session；
- Composer 向 pane 交付输入；
- Completion 使用 pane 的 cwd、shell、remote authority；
- pane 关闭后清理 live subscription，历史 row 仍可 Resume。

禁止各自维护“当前 pane”。

## 分阶段路线

### Phase 0：架构与质量基线

定义 PaneIdentity、EnvironmentId、InputDelivery、AgentActivityEvent；静态检查锁定唯一写入 primitive；建立 UI interaction/snapshot 基线。

### Phase 1：Composer MVP

单 pane、多行纯文本；pane 草稿隔离；Cmd+Enter；Shift+Enter；本地/远程统一 delivery；bracketed paste；断线失败可恢复。

### Phase 2：Session Navigator + Resume

Codex/Claude 本地发现；历史/live 合并；typed argv resume；后续扩展 remote provider scan。

### Phase 3：Activity Bar MVP

复用现有 AgentStatus 和 OSC 777；展示状态、session、耗时和 message；无可靠 tool event 时不伪造 tool detail。

### Phase 4：Codex/Claude Tool Activity

provider adapter、tool lifecycle、permission/waiting、详情抽屉、payload 清洗和容量限制。

### Phase 5：Completion Engine 重构

统一 candidate/source/ranking；保留现有 signature/generator；generation cancellation；rich details；兼容性和延迟基准。

### Phase 6：Repo-aware Completion

npm/Cargo/Make/just 等 source；缓存、watcher、本地/远程 Host backend、source diagnostics。

### Phase 7：可选本地预测

默认关闭；不依赖 BYOK；不自动执行；明确标注；优先级低于确定性结果。

## Spec 与质量门

后续新增：

```text
docs/specs/COMPOSER_SPEC.yaml
docs/specs/AGENT_ACTIVITY_SPEC.yaml
docs/specs/COMPLETION_SPEC.yaml
docs/specs/PANE_AUTHORITY_SPEC.yaml
```

Composer matrix：pane switch、target close、remote reconnect、bracketed paste、IME、exactly-once、stale async。

Activity matrix：ordering/dedup、unfinished tool、pane close、provider restart、late session ID、permission resume、redaction、本地/远程隔离。

Completion matrix：quote/escape、pipe/redirect/subshell、Unicode range、stale generator、cwd mutation、dedup、failed-history penalty、remote source、延迟和数量上限。

新增静态检查：

- `script/check_input_delivery_boundary`；
- `script/check_agent_activity_adapters`；
- `script/check_completion_sources`；
- `script/check_i18n_parity`。

## 推荐优先级

1. Composer MVP；
2. Session Navigator + Resume；
3. Activity Bar 基础状态；
4. Completion Engine 重构；
5. Codex/Claude tool detail；
6. Repo-aware completion；
7. 可选本地预测。

Composer 高价值、低耦合；Navigator 定义主导航；Activity 依赖可靠事件；Completion 位于关键输入路径，应在 authority 和 delivery 稳定后重构。

## 首轮明确不做

- 内嵌模型聊天和模型 BYOK；
- 从 terminal screen 猜完整 ToolCall；
- 自动执行预测命令；
- 全量复制 provider transcript；
- 每个 provider 各建一套 UI 状态机；
- 用 Composer 替换 terminal 原生输入。
