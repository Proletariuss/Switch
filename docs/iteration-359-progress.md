# Iteration 359：OPTIONS provisional response 解析与租约完成迁入 Rust

本轮将 OPTIONS 心跳响应的 SIP 状态行解析及 1xx/最终响应区分移入 Rust。Go 将完整序列化的响应帧交给既有 IMS codec 私有通道；Rust 校验报文内 Call-ID、CSeq、OPTIONS 方法及 session generation。1xx 临时响应保持事务租约有效，后续最终响应才完成租约并返回状态码与 `final` 标记。Go 不再读取 SIP 状态码决定是否结束等待，继续负责已注册 IMS stream、SIP 消息分发、超时/取消、写入及健康状态副作用。既有 `complete` 操作仍兼容，未新增路由。

回归覆盖 180 后续接 200 的租约保留/完成、最终非 2xx 状态传递、有效 SIP 帧及超时/闭合通道。专项 Rust 测试通过；Go `./internal/vowifi/imscore` 与 `./cmd/proletarius-macos` 测试通过。完整迁移门禁通过：Rust workspace **1,365 passed / 3 ignored**，Go `go vet ./...` 与 `go test ./...`、Swift BLE operation-proof/manager 回归以及既有 carrier、USB、radio、HTTP、codec、SDP、MESSAGE parity 均通过；契约扫描 **343 method/path（Rust 283 / Go 60）**、127 Swift 调用点、353 审阅范围通过。日志：[iteration-359-migration-gates.log](../build/optimization/iteration-359-migration-gates.log)。

P3 +0.025 计划人周等值，累计 **1.740 / 8.0**；总计 **16.220 / 47.5 = 34.1474%**（显示 34.1%，两位小数 34.15%，估算误差约 ±3 个百分点）。仍由 Go 持有 IMS socket/stream、SIP parser 与网络/业务生命周期；未做实机、SIM、短信或运营商验收，也未生成 DMG。
