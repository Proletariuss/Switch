# Iteration 410：外呼 SIP 请求 wire 编码迁入 Rust

生产通话路径中的 `INVITE/ACK/CANCEL/BYE/PRACK` 继续由 Go call/dialog 状态机选择请求语义、目标 URI、有序 header 和 body；wire serialization 现交由既有认证 IMS codec endpoint 的 Rust SIP serializer 执行。Rust 对 tel URI、PRACK、核心事务头、注入、重复头、Content-Length 与消息大小实施校验。Go 保留本地/远端 tag 与 route set 管理、CSeq/branch、SIP 写队列、socket/响应分派、dialog、媒体和完整呼叫生命周期。生产 Rust 回调无可用时不静默回退；未增加 HTTP 路由、卡片或网络 side effect。

Go/Rust 字节级 parity 通过，包含 SDP INVITE、带 route set 的 PRACK、保持原 INVITE branch/CSeq 的 CANCEL；另有生产配置回调、Rust `tel:`/PRACK handler、错误边界和通用 codec 测试。完整迁移门禁、Rustfmt、Go test/vet、Swift BLE regressions、carrier 2,722 例 replay、既有 parity 及 365-scope 契约 diff 均通过。日志：[iteration-410-migration-gates.log](../build/optimization/iteration-410-migration-gates.log)，SHA-256 `40008a34ecc9b33fa5557796d34c1a5e277688cddcf20984a7b0cbd9d3f21b93`。

P3 +0.025，累计 **2.740 / 8.0（34.25%）**；全项目 **17.445 / 47.5 = 36.7263%（显示 36.73%）**。无 IMS/运营商实机呼叫验收；当前 3.08/build308 DMG 截止 Iteration 409，不包含本轮。Go 仍构建、嵌入及运行，因此全 Rust 目标远未完成。
