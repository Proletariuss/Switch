# Iteration 441：Rust IMS actor/SMS 离线闭环复核

日期：2026-10-13

## 验证结果

- 复跑 `ims_sms_transactions::tests::registered_tcp_actor_and_consumer_archive_then_exchange_real_rp_ack`：通过。该 fixture 覆盖已注册 TCP SIP runtime、单一 actor 接收 SIP MESSAGE、先提交归档再发送 RP-ACK，以及取消后等待 actor 清理。
- `cargo test --locked --manifest-path rust/Cargo.toml -p proletariusd`：**1447 passed / 3 ignored**，耗时 20.92 秒。
- 构建有 2 条与 gated native session 类型字段相关的 dead-code warning；没有测试失败。

## 迁移状态

本轮没有增加生产 owner 接管，也没有改变加权进度（**37.99%，18.045/47.5**）。离线集成 fixture 不等于真实 IKE/IPsec+IMS、eSIM AKA、SIP over ESP 或真实 SMS 验收；`native_session_owner_readiness` 的 keyed actor、protected IMS runtime 与 live SMS/call runtime gates 继续关闭。`/api/v2/vowifi/native-session-plan` 目前只是持有并释放读卡器 lease 的配置/传输诊断端点，尚未启动长期 session actor。不得将此离线通过计作 VoWiFi 已接管。

本轮未改动业务代码，没有连接设备、读卡器、SIM 或运营商网络；9eSIM `6982` 继续排除。没有生成新 DMG。

## 后续

下一切片需要把 Rust session lifecycle 做成有代次的启动/停止生产编排：持有实际 `NativeSessionContext` 和 lease，启动由当前 session tunnel 创建的 IMS transport，接入 `NativeSessionServiceOwner::launch`、IMS actor 与 SMS consumer，并提供查询/停止和清理确认。开始前先补齐 profile 到 session policy、IMS endpoints、SMS subscription/归档 identity 的权威数据来源；生产 gate 仍需在安全停止、lease 丢失和结果未知场景的离线测试通过后逐项评估。
