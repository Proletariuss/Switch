# Iteration 408：USSD SIP 内容类型与 multipart/XML 解码迁入 Rust

生产读卡器 VoWiFi 的 USSD 入站响应现将原始 `Content-Type` 和 SIP body 交给 Rust IMS codec；Rust 在有界输入内解析直接 USSD XML 或 `multipart/mixed`，按 Go 原路径查找前 8 个 part、保留 quoted boundary 与传输编码行为，并返回已有字段或稳定错误类别。Go 仍负责 SIP 接收/回包、响应队列、USSD dialog 与 session 生命周期。调用复用现有 app-token 认证的 IMS codec endpoint，没有新增 HTTP method/path，也没有卡片、网络或 SIP I/O 权限。

测试覆盖 direct XML、Rust 构造的 multipart、quoted boundary、第 8/第 9 part 边界、内容缺失/类型错误、无效 XML、运营商错误码、超长响应及 Go/Rust 差分；差分 fixture 已纳入 `scripts/test-migration-gates.sh`。Rust workspace、Go 全包测试与 vet、Swift BLE regressions、跨语言 parity 均通过；契约 diff 通过：346 method/path（Rust 286 / Go 60）、127 Swift 调用点、362 个 reviewed scopes，无新增路由。门禁日志：[iteration-408-migration-gates.log](../build/optimization/iteration-408-migration-gates.log)，SHA-256 `4c1bdb94fcab7d7cae487120783f67ea61ee0ec359bc21a1f41c6cc580dc8a7c`。

P3 +0.025，累计 **2.690 / 8.0**；全项目累计 **17.395 / 47.5 = 36.6211%（显示 36.62%）**。无 IMS/运营商实机验收；本轮未生成 DMG。当前 3.07/build307 DMG 是用户上一轮明确要求制作的，封装源码截至 Iteration 407，不包含本轮。
