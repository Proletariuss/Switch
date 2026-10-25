# Iteration 456：将 REGISTER 续期信号接入短信事务排空状态

日期：2026-10-13。所有修改位于 `/Users/proletarius/Desktop/Switch`。未连接读卡器/号卡、未发送短信、未访问运营商网络；本轮未生成 DMG。用户刚刚要求生成的 Iteration 455 DMG 已另行构建并验证，不含本轮源码修改。

## 改动

- Rust `SessionConsumer` 现在单独识别 `RegistrationRenewalDue`，不再把它交给普通短信 archive 路径当作未知 SIP 消息。
- 到期通知携带当前未完成 delivery-report transaction 数量；若已有待确认报告，最后一笔完成时 `ReportFinished` 标记 `renewal_reports_drained=true`，让上层能观察到报告排空边界。
- 此状态仅描述短信 delivery-report 队列，不代表 SIP actor 已 quiesce，也不允许单独关闭/重建 IKE/IMS。生产 native lifecycle 尚未接入这一通知，所有相关 production gates 保持关闭。
- 加入三个状态测试：空报告队列、多个报告按序排空、普通报告完成时无 renewal 不触发排空信号。

## 验证

- `cargo test --locked --manifest-path rust/Cargo.toml -p proletariusd`：**1,459 passed / 0 failed / 3 ignored**。
- `cargo fmt --all --manifest-path rust/Cargo.toml --check` 与 `git diff --check` 通过。
- `python3 scripts/check-rust-migration-contracts.py --diff` 通过；契约 census 保持 354 method/path（Rust 293 / Go 61）、127 Swift 调用点及 378 审阅范围。本轮不涉及 HTTP/API owner，不增加迁移权重。

## 阶段状态

生产 REGISTER 续期及同会话重建仍未实现。Native IMS、live SMS/call gates 继续关闭，项目加权进度保持 **37.99%（18.045/47.5）**；不得将此轮事件状态测试表述为实际续期、掉线恢复、VoWiFi 连续在线或实机验证。
