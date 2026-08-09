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

## 用户反馈追踪纪律

- 用户提出可执行的产品、交互、架构或质量意见后，切换话题前必须追加到 `docs/quality/development-tracker.yaml` 的 `feedback` 清单，禁止只依赖会话记忆。
- 每条反馈使用稳定 `FEEDBACK-YYYYMMDD-NNN` ID，并记录原始结论摘要、状态、涉及区域、决策和下一步。
- `captured` / `triaged` 反馈只是需求入口，不是实现正本；开工前必须按强制开发顺序提升为对应 SPEC、milestone item 和 matrix 行。
- 反馈不得静默删除；不采纳或被替代时使用 `declined` / `superseded`，并写清原因或替代 ID。

## 工程架构原则

### 以模块内聚为荣，以无关耦合为耻

- 会一起变化的东西放在一起，无关的东西保持可分离。
- 修改或删除一个关注点，只应该动一个地方。

### 以层次分明为荣，以依赖混乱为耻

- 业务逻辑不应直接依赖真实 I/O，用假数据在测试里也应该能跑。
- 更换 backend 不能影响业务逻辑。
- 每个 app 绝不能调用自己的 HTTP endpoint。

### 以能力复用为荣，以复制重造为耻

- 每种能力只保留一个实现。
- 新调用方应该接入已有接口，而不是复制一份。

### 以单元扩展为荣，以功能膨胀为耻

- 新增一个 feature 应该只是“加几个文件，再加一行注册”。
- 删除一个 feature 也不应该改动其他 feature。

### 以范式统一为荣，以各自为政为耻

- 每件事只有一种既定做法。
- 新人照着已有模式写即可，不需要重新选择方案。
- 测试不应依赖 server 或 DB。

### 以及时删码为荣，以留存旧账为耻

- 代码一旦没人使用，立即删除。
- 没有调用方的代码不能上线；“以后也许有用”不构成保留理由，Git 会保存历史。

### 以架构简洁为荣，以过度防御为耻

- 避免编写过度防御和无依据的兜底代码。
### 以规范驱动为荣，以先写代码为耻

- 架构、产品和用户可观察行为变更必须依次完成 `SPEC → tracker → matrix → static CHECK → failing TEST → IMPLEMENTATION → VERIFY`。
- 进入 IMPLEMENTATION 前必须逐条念出并对照本节八项原则；任何一项不满足时先修正设计，不得以已有半成品或时间压力跳过。
- 参考项目能力迁移必须先审计当前规范、tracker、回归测试和修复提交，提取历史失败语义；禁止只复制最简单可见外壳。

## 验证纪律

- Rust 改动至少运行相关 package 的定向测试。
- 不得通过删除断言、忽略测试或扩大 allowlist 来“修复”质量门。
- 长时间构建应报告真实状态；不要并发启动多个 Cargo 进程争抢 build lock。
- 修改用户的 `~/.config/agentty`、SSH 配置或真实会话前必须备份，测试优先使用临时目录。

## 打包产物清理纪律

- 每个桌面打包入口必须在创建 stage 或输出文件之前删除该平台、该架构的旧 stage 与旧最终包；禁止覆盖式复用旧包。
- 清理范围必须精确限定为当前平台和架构，不能删除同一矩阵 job 中已经生成的其他格式产物（例如 Linux tarball 与 AppImage）。
- 打包静态门禁必须锁定这些清理动作；新增或修改打包入口时同步更新 `script/check_package_cleanup`。
## 开发早期重构原则

- 根目录 `DEVELOPMENT.md` 是开发阶段、债务策略、产品模型和版本策略的正本。
- 当前阶段不追求最小改动；遇到不合理模型应先更新 SPEC/YAML tracker，再完整替换并删除旧路径。
- 内部 Rust API 暂不承诺兼容。用户数据和外部协议兼容必须由 spec 明确要求。
- 临时债务必须进入 `docs/quality/development-tracker.yaml`，包含 owner、removal condition 和验证方式。
- 新模型验证通过后必须执行 DELETE OLD PATH，禁止长期双轨。
