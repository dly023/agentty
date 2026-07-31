# Agentty 文档与质量正本

Agentty 的开发采用规范驱动质量门。文档不是实现后的说明，而是行为修改前的输入。

## 目录

- `specs/PRODUCT_SPEC.yaml`：产品边界与非目标。
- `specs/SSH_SPEC.yaml`：OpenSSH 配置兼容、认证与错误呈现契约。
- `specs/SESSION_NAVIGATOR_SPEC.yaml`：Agent 会话发现、列表与 Resume 契约。
- `specs/I18N_SPEC.yaml`：中文/英文原生本地化契约。
- `quality/traceability.yaml`：spec → source → check → test → runtime evidence 追踪矩阵。
- `quality/CHECKLIST.md`：实现和评审清单。

## 修改流程

1. 先修改相应 SPEC，并分配稳定 ID。
2. 在 `quality/traceability.yaml` 增加或更新矩阵行。
3. 增加静态检查或失败测试。
4. 修改实现。
5. 运行 `./script/presubmit quick`。
6. 大型或交付变更运行 `./script/presubmit full`，并把结果写回 matrix 的 evidence。
