# Iteration 330：身份读取支持借用精确读卡事务

2026-10-10。改动及验证仅在 `/Users/proletarius/Desktop/Switch`；未连接实体读卡器、SIM 或运营商网络，未生成 App/DMG。

## 变更

- Rust `/api/v2/reader/identity` 现在接受明确配对的 Rust reader workflow capability 与 Rust PC/SC transaction binding，并在这两个能力的约束下读取；只给其中一个或 reader 不匹配会拒绝。此路径不会再尝试在 Go 已持有卡片工作流时另行抢占读卡器。
- 保留没有工作流参数的既有 Rust native-owned 读取行为及其错误分类，避免改变原接口调用语义。
- Go native identity transport 现在校验当前 reader/generation workflow capability，并随请求传递精确 PC/SC binding；定向测试覆盖成功载荷和 Rust busy/absent/unavailable 错误映射。

## 验证

- Rust reader identity 定向测试：**3 passed**；Go native identity 定向测试通过。
- 最终源码全量 `scripts/test-migration-gates.sh` 通过：`proletariusd` **1,317 passed / 3 ignored**，其余 Rust workspace crate tests 也通过；Go 全仓测试与 vet、Swift BLE 38 组生产事件序列、2,722 carrier replay、USB/radio/HTTP/codec/SDP/MESSAGE parity 全部通过。日志：[Iteration 330 migration gates](../build/optimization/iteration-330-migration-gates.log)。
- `cargo fmt --check` 与 `git diff --check` 通过。

## 迁移边界与进度

这是让现有 Rust identity reader 能在明确的 Rust 读卡 workflow 中安全复用，不等于 Go 已退出身份初始化。Go `RefreshUSIMInfo` 尚未调用此适配器；完整 USIM/ISIM、MSISDN、SMSC、PNN/SPN 元数据的 Rust 生产读取，以及 9eSIM BLE 身份初始化仍待接通和验证。因此本轮不计全 Rust 加权完成度，当前仍按 **15.395 / 47.5 ≈ 32.4%** 报告。未做实机验证。

下一步把身份读取接入 Go 的连接/刷新流程，先覆盖 PC/SC，并保留 Rust 生命周期的 prepare/read/commit 原子顺序；随后在同一生产 owner 中接通 BLE bridge transport，移除 Go USIM identity APDU 读取和旧 helper。只有生产调用与代次、切卡错误语义差分通过后，才核算工作包进度。
