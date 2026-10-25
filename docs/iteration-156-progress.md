# Iteration 156：IMS 入站短信解码与 BLE 控制路由迁入 Rust

日期：2026-10-08。修改范围均在 `/Users/proletarius/Desktop/Switch/src`；持续目标仍是让全部后端业务由 Rust 承担。本轮未生成 DMG，也未访问读卡器、SIM、运营商网络或真实 IMS 会话。

## 迁移内容

Rust 在现有认证私有 `POST /api/internal/ims/sms-transactions` 增加 `decode_incoming_delivery` action，解析有界 RP-MT-DATA、SMS-DELIVER 与 STATUS-REPORT，并组装发送方、正文、服务中心时间戳、拼接信息和送达状态。畸形 RPDU 会在进入耐久回调前拒绝；有效 RPDU 携带不支持或无法解码的 TPDU 时保留原始事件及解码状态，维持旧原始归档行为。Go 的解码 callback、RPDU/TPDU parser 和双路 fallback 已移除；生产 IMS session 强制使用 Rust coordinator。Go 仍负责初始 SIP 200、当前注册 stream、durable callback 次序，并仅在持久化成功后发送 RP-ACK。

BLE `POST /api/reader/ble/scan`、`connect`、`disconnect` 现由 Rust 执行公开路由认证、请求校验、Swift CoreBluetooth UDS 调用、结果投影及有界等待。Go 原三个公开 handler 已移除，只保留带随机 token 和过期时间的共享读卡器操作 gate lease；Swift 仍是 CoreBluetooth 系统桥。旧 Go ServeMux 方法合同已写入独立冻结 fixture，避免公开路由移交后兼容对照样本随 Go handler 一并消失。读卡器选择、BLE APDU/LPA 会话和完整 reader 生命周期仍有 Go 依赖。

## 验证

- `scripts/test-migration-gates.sh`：通过。Rust workspace **1,428通过、0失败、6忽略**（`proletariusd` **929通过、3忽略**）；Go 全仓 `go vet ./...` 与 `go test ./...` 通过。
- Carrier replay **2,722项**、USB status、radio policy、HTTP/HEAD wire parity、PCMA历史fixture、SDP与AMR parity 均通过。
- Rust migration contract 工具 **43项自测**、清单 `--diff`、Rustfmt及 `git diff --check` 通过。清单现为 **288条 method/path**（Rust **219**、Go **69**，含35个 HEAD alias）、127个 Swift HTTP 调用点、257个审阅范围。
- `--require-rust-only` 按预期失败：默认 Go fallback 仍存在，145条 route 仍有 Go 所有权或依赖。此项不阻止当前切片，但说明总体迁移没有完成。

本轮保守计入 P2B **+0.025计划人周**，由4.850升至**4.875/8.0（60.94%）**；P3 **+0.025计划人周**，由1.490升至**1.515/8.0（18.94%）**。累计由13.420升至**13.470/47.5≈28.4%**，估算误差约±3个百分点。Rust handler 和测试通过不代表实机 BLE、真实短信投递、IMS session/VoWiFi生命周期或全 Rust 构建已验收。
