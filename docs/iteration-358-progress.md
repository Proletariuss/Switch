# Iteration 358：VoWiFi OPTIONS 状态判定迁入 Rust

本轮将 OPTIONS 心跳最终响应的成功判定迁入 Rust。Go 仍通过原有 SIP stream 接收并解析报文，使用 Rust transaction lease 验证 Call-ID/CSeq 等关联后，把最终 SIP 状态码交给 Rust heartbeat supervisor。Rust 独占将 2xx 判为成功、将其他最终状态判为 miss，并返回继续、延迟确认、恢复或 teardown 决策；Go 按该决策执行现有 readiness、等待和关闭动作。RPC 不再携带 Go 推导的 `succeeded` 布尔值，避免两端各自决定一次。

拒绝响应仍使用原来的确认探测策略：单次失败暂时撤销 ready 并按 Rust 返回的延迟重试，二次失败才退出会话；验证错误、传输超时和取消保持各自现有错误语义。Rust 回归覆盖 2xx、403 两次失败、状态范围校验、过期/重复 probe 和代次隔离；Go 回归验证 403 由 Rust 决策触发第二次探测且诊断保留 SIP 状态。

验证：定向 Rust 心跳测试 7 项通过；VoWiFi 与 daemon bridge 定向 Go 测试通过；完整迁移门禁通过（Rust workspace 1,363 passed / 3 ignored，Go `go vet ./...` 与 `go test ./...`，Swift BLE proof、38 组 BLE manager 回归及 carrier、USB、radio、HTTP、codec、MESSAGE 差分）。契约审计通过：343 method/path（Rust 283 / Go 60）、127 个 Swift 调用点、353 个审阅范围；未新增对外路由。完整门禁日志：[iteration-358-migration-gates.log](../build/optimization/iteration-358-migration-gates.log)。

P3 增加 0.025 计划人周等值，累计 **16.195 / 47.5 = 34.0947%（显示 34.1%，两位小数 34.09%）**。尚未迁移 Go IMS socket/stream 和 SIP parser、在线短信/通话生命周期及硬件会话；本轮没有实机读卡、发信或呼叫，也未生成 DMG。
