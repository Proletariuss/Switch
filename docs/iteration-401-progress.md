# Iteration 401：入站 USSD 对话路由判定接入 Rust

2026-10-12

入站 SIP 请求现将 method、SMS-only/呼叫 handler 可用事实、request Call-ID 和活动 USSD 对话 Call-ID 一并发送到既有认证 `POST /api/v2/call/invite-policy`。Rust 仅在两个 Call-ID 均非空且完全匹配时返回 `ussd_dispatch`；否则保持原有 MESSAGE/OPTIONS/ACK/呼叫方法及未知方法的分派规则。没有新增 HTTP 路径或改变网络报文格式。

Go 继续拥有 SIP parser、TCP 接收器、同连接响应、USSD dialog 与队列、短信/呼叫 consumer 和生命周期。Rust 动作到达后 Go 将消息提交到现有有界 USSD channel；队列已满/对话结束时的 503/481 行为仍由当前 adapter 执行。若 USSD 对话在策略 IPC 期间结束，Go 清除过期 active Call-ID 并再次调用 Rust，让普通方法策略重新裁决。没有启用影子处理或重复网络/卡片操作。

验证：Rust 入站 SIP policy 测试4项通过（包含精确匹配、空值/不匹配、危险 Call-ID 拒绝）；Go imscore 与 native adapter 路由/认证测试通过，并对这两个 Go 测试范围执行 race 检查；`scripts/test-migration-gates.sh` 全量通过（Rust workspace 2003 passed / 6 ignored、Go vet/全包测试、Swift BLE 回归和跨语言 parity）；API 契约 `--diff` 通过，346 method/path（Rust 286 / Go 60）、127 Swift 调用点、360 审阅范围未增加；Python 审计自测43项通过。完整门禁日志 [`iteration-401-migration-gates.log`](../../build/optimization/iteration-401-migration-gates.log)，race 日志 [`iteration-401-go-race.log`](../../build/optimization/iteration-401-go-race.log)。

P3 +0.025，累计 2.515 / 8.0；全项目 17.220 / 47.5 = 36.3%（两位小数36.25%，估算误差约 ±3 个百分点）。这只迁移一段路由决策，不代表 USSD IMS dialog、SIP 接收器或 Go event loop 已退出；无 IMS/设备/运营商实机验收。3.03/build303 DMG 冻结于 Iteration 400，不包含本轮。本轮不生成 DMG。
