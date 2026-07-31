# 变更检查清单

## 开始前

- [ ] 确认修改属于哪个 domain spec。
- [ ] 分配或复用稳定 spec ID。
- [ ] 明确 canonical source、调用方和禁止的平行路径。
- [ ] 在 traceability matrix 中声明验证方式。

## 实现时

- [ ] 先写失败测试或可复现脚本。
- [ ] 一次只改变一个行为变量。
- [ ] 不读取或修改无关用户数据。
- [ ] 错误信息可行动且不泄露 secret。
- [ ] 本地和远程 authority 不混淆。

## 完成前

- [ ] `cargo fmt --check`
- [ ] 相关定向测试通过。
- [ ] `./script/presubmit quick` 通过。
- [ ] matrix evidence 已更新或明确为 planned/pending。
- [ ] 用户可观察行为与 SPEC 一致。
