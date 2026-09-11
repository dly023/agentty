# Agentty 开发约定

Agentty 处于早期架构开发阶段。优先保证产品模型、身份和状态归属正确，不为缩小 diff 长期保留不合理的内部设计。本文件是开发阶段、债务策略、产品模型和版本策略的入口；具体行为以领域 SPEC 为准，进度和验证证据只维护在 tracker 中。

## 从哪里开始

- 稳定工程边界：[AGENTS.md](AGENTS.md)。
- 领域导航：[工程文档索引](docs/README.md)。
- 当前需求、决策、里程碑和未完成项：[development-tracker.yaml](docs/quality/development-tracker.yaml)。
- 契约到源码、静态检查和测试的映射：[traceability.yaml](docs/quality/traceability.yaml)。

历史归档和其他项目用于提取失败语义与回归案例，不是当前实现或完成状态的正本。先核对当前调用链；不要直接复制归档中的产品模型、版本常量或函数名。

## 产品模型与状态归属

- UI 产品名为 Agentty；技术内核、Rust package、CLI 和 server 保留 tty7 血缘，按[品牌契约](docs/specs/PRODUCT_BRANDING_SPEC.yaml)区分两者。
- 面向用户的 workspace 表示机器环境，不是目录。多台机器在左栏并列，展开其会话；Pin、折叠、扫描历史和显式断开围绕机器展开。目录属于会话执行上下文，不再作为侧栏的一级归属。
- 左栏可展示多台机器，不意味着内容区可以混用执行权限。窗口、pane、session 与目标 Host 的身份必须显式传递；终端内手工运行 `ssh` 不自动改变产品的机器归属。
- managed remote 的机器树、pane facts、provider 数据、会话用户状态和 hooks 由目标 Host/daemon 提供。GUI 的客户端配置、缓存和待提交意图不能在重连或读取失败时变成远端事实；远端写入只走带身份及 ownership 前置条件的 canonical HostOps/ControlRequest。
- 标签、进程和 provider 会话不是同一个生命周期。显示历史记录不等于附着进程，Rejoin 不启动新进程，Resume 必须显式发起；断线或没有观测到运行状态不能推断会话已停止。

允许和禁止的具体路径见[机器与会话契约](docs/specs/MACHINE_RAIL_SPEC.yaml)。产品行为待确认时，记录问题及影响，仅暂停依赖该决定的步骤，不把猜测写成已接受的实现规则。

## 开发顺序与债务

`SPEC → tracker → matrix → static CHECK → failing TEST → IMPLEMENTATION → VERIFY → DELETE OLD PATH`

实现前明确 action、canonical owner、身份、状态变化、持久化/投影和可见结果；逐项对照 AGENTS 的八项架构原则。每个异步阶段都要说明成功、失败、取消、超时、旧回包和载体缺失时的行为。

- 会一起变化的代码放在一起；业务逻辑与真实 I/O 分层，通过已有 backend 和窄接口扩展。
- 不新增第二份可写正本、平行恢复入口或伪造成功的兜底。重写必须继承旧路径的身份、标题、绑定、生命周期与 ownership 约束。
- 新模型验证通过后删除旧路径及无人调用的兼容包装，不以“以后可能用”保留死代码。
- 内部 Rust API 暂不承诺稳定；用户数据和外部协议的兼容要求由 SPEC 明确，不因内部重构任意破坏。
- 临时债务必须在 tracker 中登记 owner、移除条件和验证方式，不能只留一个无归属 TODO。
- 用户可执行反馈进入 tracker 的稳定 feedback ID；未采纳或被替代的反馈保留理由，不能静默删除。

参考 Ashide 的 Session Discovery、Tab Lifecycle 或 Completion 前，核对[失败语义资产矩阵](docs/quality/ASHIDE_FALLBACK_ASSET_MATRIX.yaml)。迁移的是不变量与测试，不是重型关系框架或 BYOK 设计。

## 验证与完成声明

复用唯一入口，命令从仓库根目录执行：

```sh
./script/presubmit quick
./script/presubmit full
```

Rust 改动先运行相关 package 的定向测试，使用 `--locked`；一次只运行一个 Cargo 进程。toolchain 与检查顺序见[质量门契约](docs/specs/QUALITY_GATE_SPEC.yaml)及 [Rust 检查 skill](.agents/skills/agentty-rust-check/SKILL.md)。

检查失败时区分行为失败、门禁误判/漏检、环境失败和未执行验收。不要通过删除断言、增加忽略项或扩大白名单取得绿灯。修检查器也要先用负向夹具证明预期违规确实被拒绝。

验证期间冻结相关构建输入，记录命令、退出码及对应源码/产物身份；不要将测试 A、构建 B、运行 C 混为一次验收。长构建或磁盘不足时报告实际阶段，只清理精确、可再生成的构建产物。

原生 UI 验收使用隔离配置和可丢弃会话。先核对源码、可执行文件、bundle、PID、窗口及目标身份，再派发输入；菜单关闭、窗口移动或焦点变化后重新定位。采集到截图不是行为通过，模拟 GPUI 测试不是原生窗口测试，macOS 测试不是远端或跨平台验证。

按场景保留动作前后画面、准确会话/载体身份、日志与可观察结果；恢复、关闭、删除、滚动分别需要对应断言。权限或驱动失败停在实际失败阶段，不反复触发认证、不声称已测到产品。完成报告分别列出已实现、已验证、失败和未执行项。

修改真实用户配置或会话前必须获得对应授权并备份；默认使用临时目录。正式应用、当前开发会话和真实 provider 文件不是破坏性测试夹具。

## 版本、打包与上游

包版本由根 `Cargo.toml` 的 `workspace.package.version` 与锁文件确定，不在本文另维护一个固定版本。不要套用历史归档的版本约定，也不因普通开发提交自动递增版本。

发布/下载版本、目标协议版本、源码身份和平台 bundle 版本各有用途，遵循[品牌与产物契约](docs/specs/PRODUCT_BRANDING_SPEC.yaml)。源码指纹不等于二进制哈希，macOS bundle 数字版本不等于完整诊断版本；不要混用或手改 stamp 来绕过一致性检查。

打包清理限定当前平台、架构和格式的旧 stage/旧最终包，不能删除同一构建矩阵中的其他格式产物。源检查或模拟签名成功不能替代真实包、helper、签名、安装和启动验收。

保留上游血缘不意味着整体合并历史实现或最新上游。按明确基线评审行为、失败语义和测试，再沿 Agentty 当前 canonical 路径吸收；不移植冲突的目录分组或状态正本。Git 元数据不可用时使用当前源码与已有构建身份，不虚构 commit、远端分支状态或发布结果。提交、推送、签名、发布和真实环境部署遵循各自授权范围。
