# Iteration 455：将 REGISTER 续期时点送入 Rust SIP 事件流

日期：2026-10-13。修改范围限于 `/Users/proletarius/Desktop/Switch`。未访问读卡器/号卡、未发短信、未做运营商网络或实机测试；未生成 DMG。

## 改动

- `RegisteredSipRuntime` 现在同时等待 SIP 收发、待确认 transaction 超时和上轮算出的 REGISTER renewal deadline。
- 到期后发出一次性 `RegistrationRenewalDue` 事件；该通知不会关闭 SIP/IKE、自动刷新 REGISTER 或中断正在进行的短信/通话。调用者可区分续期时点并由生命周期 owner 决定何时进行受控重建。
- 通知发出后 deadline 会被消费，不会重复告警。事件与 transaction timeout 同时等待时两者仍分别保留，SIP 活动可继续。
- 测试在真实 Rust userspace TCP session harness 中验证立即到期只通知一次，随后该会话仍能完成原有短信 transaction 超时、迟到响应与下一条独立 transaction。

## 验证

- `cargo test --locked --manifest-path rust/Cargo.toml -p proletarius-vowifi`：185 项单元测试、2 项载波集成测试通过。
- `cargo check --locked --manifest-path rust/Cargo.toml -p proletariusd` 与 `cargo fmt --all --manifest-path rust/Cargo.toml --check` 通过。
- `python3 scripts/check-rust-migration-contracts.py --diff`、工具 44 项测试及 `git diff --check` 通过；HTTP census 仍为 354 method/path（Rust 293 / Go 61）、127 Swift 调用点、378 审阅范围。

## 阶段状态与交付边界

当前只把 renewal deadline 接到 Rust IMS 事件层，尚没有已启用的 production IMS consumer 协调活动通话/出站事务、受控拆建 IKE、重注册、以及保留同一 SIM/Profile 代次。native IMS production gate 与 Go runtime 退出条件仍未满足，因此 P3/P4 验收及项目整体加权进度不增加，仍为 **37.99%（18.045/47.5）**。最近 DMG `Switch-3.15-macOS27-iteration455.dmg` 早于 Iteration 454/455 源码，不包含本轮改动。
