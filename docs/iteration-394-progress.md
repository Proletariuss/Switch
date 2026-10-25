# Iteration 394：Rust 外呼 INVITE phase 成为生产投影来源

Rust outgoing-call tracker 在 begin/cancel/response/end 响应中返回其规范 phase。Go bridge 校验 phase 与 SIP 状态类别、policy action、early-media 标志及 ACK failure 状态码一致；生产 watcher 使用该 phase 投影 inviting/early/cancelling 状态，不再单独按 provisional code 推断 phase。Go 本地取消仍作为竞态栅栏，避免晚到的早期响应重新开放媒体。Go 继续负责 SIP 解析/写入、PRACK/ACK/CANCEL/BYE 报文与媒体副作用。

离线验证通过：

- 定向 Go bridge/runtime 测试覆盖 phase/status/action 一致性、重复 2xx（包括首个 2xx 缺少可用媒体后进入 failed 的 ACK-only）、Rust phase 投影和本地 cancel fence。
- Rust daemon tracker route tests 覆盖 inviting/early/active/cancelling/ended phase 回传及可靠 provisional、取消 487 与重复最终响应。
- `scripts/test-migration-gates.sh` 全量通过：[iteration-394-migration-gates.log](../build/optimization/iteration-394-migration-gates.log)，SHA256 `c0eca8214fcaf121647a43c82363fd63fd9ef066e74b04b1043f71f56d19809e`。
- 迁移契约清单保持 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、359 审阅范围；`--diff` 与 43 项审计自测通过：[diff](../build/optimization/iteration-394-contract-diff.log)、[tests](../build/optimization/iteration-394-contract-tests.log)。`git diff --check` 通过。

该生产切片将外呼事务 phase 的权威投影纳入 Rust 所有权，P5 增加 **0.025 计划人周等值**，累计 **0.100 / 8.0（1.25%）**；项目进度 **17.045 / 47.5 = 35.8842%**（显示 35.9%，两位 35.88%，估算误差约 ±3 个百分点）。没有进行实体卡、通话、短信或运营商实机验收；没有生成 DMG。SIP/媒体的实际执行和完整 Go 退出仍未完成。
