# 工程文档导航

开发入口是根目录的 [DEVELOPMENT.md](../DEVELOPMENT.md) 与 [AGENTS.md](../AGENTS.md)。本索引只指路，不重复维护功能完成率或测试结果。

## 行为契约

- [机器侧栏与会话生命周期](specs/MACHINE_RAIL_SPEC.yaml)：机器归属、历史发现、Resume/Rejoin、停止与休眠、身份和恢复。
- [客户端 I/O 边界](specs/CLIENT_IO_SPEC.yaml)：本地与远端操作归属。
- [终端滚动](specs/TERMINAL_SCROLL_SPEC.yaml)：终端视口与跳到底部。
- [产品品牌与产物身份](specs/PRODUCT_BRANDING_SPEC.yaml)：Agentty/tty7 命名、版本、helper 和打包边界。
- [质量门与工程导航](specs/QUALITY_GATE_SPEC.yaml)：检查入口、引用完整性及证明范围。

## 需求、证据与参考

- [开发 tracker](quality/development-tracker.yaml)：用户反馈、待确认决策、里程碑、验证证据与剩余工作。
- [可追溯矩阵](quality/traceability.yaml)：契约对应的源码、静态检查和测试。
- [Ashide 失败语义资产矩阵](quality/ASHIDE_FALLBACK_ASSET_MATRIX.yaml)：参考能力迁移前的历史失败与不变量。

## 执行入口

- [架构变更 skill](../.agents/skills/agentty-architecture-change/SKILL.md)。
- [Rust 检查 skill](../.agents/skills/agentty-rust-check/SKILL.md)。
- [presubmit](../script/presubmit)：quick/full 的统一入口。
- [harness](../script/check_agent_harness)：引用完整性检查，不替代行为验收。

用户使用文档从 [站点首页](index.mdx) 进入。使用文档、历史归档和交接摘要不能替代当前 SPEC 与 tracker；产品变更后的使用文档同步也需要独立核对，索引存在不代表全站内容已完成验收。
