# Iteration 221：原生 RP-ACK 报告响应生命周期

日期：2026-10-09。工作区为 Switch_MAC。

此前原生短信消费方在归档、SIP 200 和 RP-ACK MESSAGE 写入后仅返回 ResponseTicket；写入完成不能证明运营商接受 RP-ACK。本轮增加 DeliveryReportSubmission，追踪 Pending、Accepted、Rejected(status)、OutcomeUnknown。

只有当前 Primary association 上由既有 ResponseRegistry 严格匹配的 final response 才能完成报告；provisional、无匹配、其他连接及其他 ticket 不推进状态。超时或断开将尚未完成的报告标为 unknown，不自动重发、不删除短信归档；已完成结果不会被迟到 timeout 或连接关闭改写。这里是 RP-ACK 的 SIP 事务结果，不代表发出短信的送达报告，也不替代已有 SmsSubmission 对迟到确认的处理。

native_sms_consumer 返回的 ReportSubmission 封装会话 generation，另一个会话的同值本地 ticket 不能认领它。接管后的会话事件循环必须持有这些对象、将事件交给 observe，并在 teardown 时调用 disconnected；本轮没有宣称已完成该生产循环或持久报告恢复。

新增协议测试覆盖 provisional、2xx、403/503、非 Primary association、不同 ticket、超时、关闭及终态不可改写。现有内存 TCP 双端测试现在也用实际 RegisteredSipRuntime 的事件驱动报告状态：分段响应超时成为 unknown、迟到响应不能误认成功、下一事务得到匹配 final response。VoWiFi crate 165 项单元及2项集成测试通过。

完整门禁日志：`../build/optimization/iteration-221-migration-gates.log`：Rust workspace 1651 项测试、格式检查、Go全仓test/vet及既有协议兼容性门禁通过，最终标记 `migration gates passed`。整体进度仍为31.4%，原生生产启动门禁仍关闭。本轮没有实卡操作、实际短信、应用重启或DMG。
