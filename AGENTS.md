# Agentty Engineering Contract

Agentty 是轻量、终端优先的 Agent 工作台。所有架构级行为修改必须遵循规范驱动流程。

## 强制开发顺序

`SPEC → MATRIX → static CHECK → failing TEST → IMPLEMENTATION → VERIFY`

- **SPEC**：在 `docs/specs/*.yaml` 定义用户可观察行为、状态和禁止路径。
- **MATRIX**：在 `docs/quality/traceability.yaml` 建立 spec ID、源码、静态检查、测试和运行验证的映射。
- **static CHECK**：跨文件架构约束放在 `script/check_*`，默认 fail closed。
- **TEST**：修复或功能必须先有能证明行为的定向测试；不能只靠编译通过。
- **IMPLEMENTATION**：保持单一正本路径，禁止增加绕过 canonical primitive 的平行实现。
- **VERIFY**：先运行定向测试，再运行 `./script/presubmit quick`；交付前运行适用的 full gate。

## 变更分类

以下变更必须更新对应 SPEC 与 matrix：

- Agent 会话发现、Resume、provider binding、会话身份或排序；
- SSH 配置解析、认证顺序、密钥/agent/密码行为、远程部署；
- 本地/远程执行边界、持久化格式和恢复；
- i18n key、语言选择与用户可见错误；
- 进程启动、transport、materialization 等副作用入口。

纯重命名、注释和不改变行为的格式调整可以不增加 spec 条目，但必须通过已有检查。

## Agent harness 分层与上下文路由

- `.agents/skills/` 只保存稳定工作流，不保存易过期的 feature 设计。
- 当前设计和行为正本只放在 `docs/specs/`。
- 不得创建 feature-memory skill；新 skill 必须是可复用的过程型能力。
- `script/check_agent_harness` 在 presubmit 和 CI 中验证 skill、spec 与 matrix 的基本完整性。

## 参考项目资产提取纪律

- 参考 `/Users/admin/ashide` 时，优先提取失败语义、兜底不变量、generation/race 规则和测试矩阵，不复制重型框架或 BYOK 设计。
- Session Discovery、Tab Lifecycle、Completion 开工前必须核对 `docs/quality/ASHIDE_FALLBACK_ASSET_MATRIX.yaml`。
- “正常路径可用”不算完成；失败、取消、超时、stale result、partial result、断线、身份替换和恢复回滚必须有测试。
- 远程 source 失败不得读取本地数据作伪兜底；最后一次完整提交状态优先于不完整的新结果。

## 验证纪律

- Rust 改动至少运行相关 package 的定向测试。
- 不得通过删除断言、忽略测试或扩大 allowlist 来“修复”质量门。
- 长时间构建应报告真实状态；不要并发启动多个 Cargo 进程争抢 build lock。
- 修改用户的 `~/.config/agentty`、SSH 配置或真实会话前必须备份，测试优先使用临时目录。
## 开发早期重构原则

- 根目录 `DEVELOPMENT.md` 是开发阶段、债务策略、产品模型和版本策略的正本。
- 当前阶段不追求最小改动；遇到不合理模型应先更新 SPEC/YAML tracker，再完整替换并删除旧路径。
- 内部 Rust API 暂不承诺兼容。用户数据和外部协议兼容必须由 spec 明确要求。
- 临时债务必须进入 `docs/quality/development-tracker.yaml`，包含 owner、removal condition 和验证方式。
- 新模型验证通过后必须执行 DELETE OLD PATH，禁止长期双轨。
