# Iteration 407：USSD 输入/取消命令准入迁入 Rust

生产 IMS USSD 对话现在把用户输入和取消命令的状态准入交给 Rust：未建立对话拒绝命令；等待网络响应期间拒绝输入；网络请求用户输入时允许发送 INFO；已建立对话允许取消并由 Go 发 BYE。Go 仍执行 SIP 写入、编码器调用、对话状态与资源生命周期。命令通过既有认证 IMS codec endpoint，不新增 HTTP method/path。

Rust USSD 策略与 daemon operation 测试、Go callback/fallback/桥接定向测试及 Go race 测试通过；完整迁移门禁通过：[iteration-407-migration-gates.log](../build/optimization/iteration-407-migration-gates.log)，SHA-256 `0d323bfe3f1940f26f8695867d32dbdb77c2efa6014847bbd799b6d3fe7387a5`。契约审计 diff 通过：346 method/path（Rust 286 / Go 60）、127 Swift 调用点、362 个已审阅范围，无新增路由。

P3 +0.025，累计 2.665 / 8.0；全项目累计 17.370 / 47.5 = 36.5684%（显示 36.57%）。3.07/build307 DMG 冻结至 Iteration 406，不含本轮源码变更。未执行实体卡、短信、拨号或运营商实机测试。
