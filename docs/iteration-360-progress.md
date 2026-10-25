# Iteration 360：RP-ACK MESSAGE 响应观察迁入 Rust

Rust 接收完整 SIP 响应帧并负责核对 Call-ID、CSeq 和方法，区分无关响应、1xx 临时响应、2xx 接受及非 2xx 拒绝。Go 保留现有 IMS stream/socket、15 秒等待与消息写入，并依 Rust 返回的观察结果继续原有 RP-ACK 流程；没有改变超时后不自动重发的语义。

完整离线迁移门禁通过，日志见 [iteration-360-migration-gates.log](../build/optimization/iteration-360-migration-gates.log)：Rust workspace **1,366 passed / 3 ignored**，Go 全仓测试与 `go vet`、Swift BLE operation-proof/manager 回归、既有协议 parity 均通过。没有连接实体读卡器、实测短信/通话、运营商验收或生成 DMG。

本轮 P3 增加 **0.025 计划人周等值**，累计 **1.765 / 8.0**；全项目为 **16.245 / 47.5 = 34.2%**（两位小数 **34.20%**，估值误差约 ±3 个百分点）。31.4% 是 Iteration 209 的历史快照，后续旧迭代记录保留当时数值供追溯；当前统一口径见[进度复核账本](optimization/all-rust/progress-reassessment-2026-10-10.md)。
