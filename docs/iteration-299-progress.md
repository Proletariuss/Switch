# Iteration 299：修正原生会话 BLE AKA readiness 事实

Iteration 298 已把租约内 BLE USIM/ISIM AKA APDU 路径迁到 Rust `BleApduTransport`，并移除了唯一 Go AKA lease HTTP endpoint。本轮据此将 `NativeOwnerReadiness::current().ble_aka_transport_bridge` 从 false 更新为 true，并调整 readiness 缺口测试/诊断；没有启用 native session 入口。keyed lifecycle actor、protected IMS runtime 与 live SMS/call runtime 仍关闭，native VoWiFi 仍不得启动。

验证结果：`native_vowifi_session::tests::native_plan_readiness_has_lease_but_keeps_live_runtime_gates_closed` 通过（1项）；API 契约/消费者盘点通过，仍为334条契约、Rust273/Go61、127个Swift调用点及326个审阅范围。初次不带模块前缀的过滤命中0项，随后使用完整测试名重跑通过。daemon测试编译有2条既有 dead-code warning。本轮未运行完整迁移门禁。离线桥不代表真实 BLE 卡验收；不访问 SIM、不拨号/发信、不生成 DMG。整体仍为 **31.4%**，不将 readiness 元数据修正计作生产迁移工作包。
