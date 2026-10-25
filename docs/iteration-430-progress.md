# Iteration 430：IMS OPTIONS 周期调度移入 Rust

Rust IMS heartbeat session 现在持有 OPTIONS 周期调度 deadline。Go `time.Ticker` 已移除，改为通过现有认证 IMS codec UDS 长轮询等待 Rust 到期信号；请求受 session ID、generation、固定 interval 和请求 context 约束。Rust 不持锁睡眠，醒来后重验 session/代次/期限/失败状态，间隔变更拒绝，错过多个周期时丢弃积压并从当前时间重排，避免连续补发心跳。

Go 继续负责受保护 SIP socket 的请求发送和响应等待、会话中的 CSeq 更新、readiness 副作用、失败后的 3 秒确认等待与最终传输清理。这一轮没有改变探测报文、响应门限、失败确认策略或运营商网络行为。Rust schedule wait 使用可取消请求；关闭/失效会话不能唤醒出一个有效探测事件。

本轮计入 P3 +0.025 计划人周等值：周期 timer 已从 Go 生产循环删除，Rust 是生产调度 owner。全项目进度为 **17.895 / 47.5 = 37.67%**，估算误差仍约 ±3 个百分点。迁移不是 100%：Go 退出审计仍列出 6 类阻塞和 264 个生产 Go 文件。

验证：Rust daemon 测试 1,431 passed / 3 ignored；`go test ./internal/vowifi/imscore ./cmd/proletarius-macos` 通过；两包 race 测试通过；migration-contract inventory/diff 通过（349 method/path、127 Swift 调用点、372 审阅范围）；迁移工具测试 44 项通过。`scripts/test-migration-gates.sh` 全量通过，含 Go vet/全包测试、Swift BLE manager/operation proof 及多组 Rust/Go 差分门禁。记录：[iteration430-final-gates.log](../build/optimization/iteration430-final-gates.log)，SHA-256 `175abc83a3ceb6c2a06ac3cdb7627b9128094128a12f0d8517702849626b0445`。不做实体 SIM 或运营商测试，不生成 DMG。
