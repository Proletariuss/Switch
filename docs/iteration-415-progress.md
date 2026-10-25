# Iteration 415：入站呼叫状态策略强制由 Rust 裁决

生产入站呼叫要求 Rust tracker 批准初始 INVITE，并裁决重传、忙线、CANCEL、超时、BYE 以及 ringing/answered/ACK 阶段变更。Go 不再在策略 callback 缺失时使用本地 admission/state fallback；ACK 先执行 Rust tracker 转换，成功后才把 Go 观察状态设为 active 并启动媒体。缺失 policy 的回归验证只返回 500，呼叫保持 idle 且未分配媒体。Go 仍负责 SIP 收发与响应写入、SDP/媒体执行及 session/responder 生命周期。

Rust tracker 对处于 Trying 的重传将 Accept action 规范为 `accept`，和 Go adapter 的 action contract 一致，并新增 route regression。完整迁移门禁通过：Rust workspace、Go 全包测试与 vet、Swift BLE、call policy/Rust↔Go parity 均通过；IMS 呼叫包的 race 测试也通过。API 契约 census 为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、370 审阅范围，44 项扫描器测试通过。

门禁日志：[iteration-415-migration-gates.log](../build/optimization/iteration-415-migration-gates.log)，SHA-256 `f592e8fe423f3c786b199e78f2be8962098b47740ac46976c62035db170fa20d`。Rust 离线测试未替代运营商实机来电验收。

P3 +0.025 至 2.865/8.0；项目累计 17.570/47.5（36.99%，估算误差约 ±3 个百分点）。全 Rust 退出仍阻塞于 7 类 Go 构建、运行及源码依赖。本轮未生成 DMG；最近的 DMG 为 3.08/build308 Iteration 414，未包含本轮源码。
