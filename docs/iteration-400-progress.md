# Iteration 400：入站 SIP 方法分派策略接入 Rust

2026-10-12

生产读卡器 IMS 会话现在通过既有认证 `POST /api/v2/call/invite-policy` 将入站 SIP method、SMS-only 标志、通话 handler 可用性及 Allow 值交给 Rust 分类。Rust 选择 MESSAGE/OPTIONS 确认、ACK/通话派发、SMS-only 与无 handler 拒绝码，以及不支持方法的 405/Allow；Go 的生产 `dispatchIncomingRequest` 消费该动作，不再对配置了 Rust callback 的生产路径重复执行方法策略。此轮复用现有 route，method/path 契约数量没有增加。

Go 继续拥有 SIP 解析与单接收器、USSD 专用前置处理、同一 TCP association 的响应写入、短信/通话 consumer 副作用及 session 生命周期。Rust IPC 最长等待 1 秒；分类失败时非 ACK 发送 500，ACK 按协议静默丢弃；不回退到 Go 方法分类。未配置 callback 的显式兼容/测试实例仍保留旧 dispatcher。该改动不代表 Rust IMS event loop 已启用。

验证：Rust 入站方法矩阵与危险 Allow 拒绝单测通过；Go IMS consumer 顺序、405 Allow 与 Rust 失败关闭定向测试通过；Go→Rust adapter 的认证头、请求事实和结果校验测试通过。完整 `scripts/test-migration-gates.sh` 通过（Rust workspace 1407 passed / 3 ignored、Go vet/全包测试及 parity）；门禁日志 [`iteration-400-migration-gates.log`](../../build/optimization/iteration-400-migration-gates.log)，SHA-256 `f4eec51b22e5f8498747084f9515ccb581efb0fb97838277e429b94c0676a91e`。契约 `--diff` 与 43 项扫描器测试通过：346 method/path（Rust 286 / Go 60）、127 Swift 调用点、360 审阅范围；无实机/运营商呼叫验收。

P3 增加 0.025，累计 2.490 / 8.0；全项目累计 17.195 / 47.5 = 36.2%（两位小数 36.20%，估算误差约 ±3 个百分点）。零 Go 构建/运行依赖、native IMS loop、来去电生命周期、IKE/IPsec、模块/读卡器 owner、实机与72小时验收仍未完成。本轮未操作读卡器、号卡、短信或运营商网络。3.02/build 302 对应 Iteration 399；3.03/build 303 已生成，DMG 校验、挂载应用版本/架构读取与 strict codesign 验证通过。SHA-256：`de2d3af97a6ada152803ac3f79db3d202dcb14f2166c1c3e42e95d9b59ac38ae`。
