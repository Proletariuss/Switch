# Iteration 388：EAP_DIRECT 初始 REGISTER Digest 迁入 Rust

生产读卡器 VoWiFi 会话现在将当前已验证 EAP-AKA 的 RES、RAND、AUTN 交给 Rust IMS codec，Rust 在不触卡、不再次 AKA 的前提下完成 eap_direct 首次 REGISTER 的 AKAv1-MD5 摘要与 Authorization 头构造。Rust 返回结果必须是有界、无换行且以 Digest 开头的头字段；Rust 不可用或结果异常时生产路径失败关闭。Go 保留未配置 Rust callback 的兼容实现。实现保持旧 wire 格式（参数顺序、无空格逗号、首 16 字节 RAND/AUTN nonce、RES 原始字节摘要）以及旧路径允许的 1–16 字节 RES 行为。

验证通过：Rust authorization 单测 **3/3**，daemon codec operation 测试 **1/1**；Go `imscore` 与 macOS backend packages 测试通过。完整 `scripts/test-migration-gates.sh` 通过，包括 Rust workspace、Go 全仓 test/vet、Swift BLE/manager 回归、HTTP method/HEAD 与 IMS codec/SDP/MESSAGE 差分。迁移契约 `--diff` 通过：343 个 method/path（Rust 283 / Go 60）、127 Swift 调用点、359 个审阅范围。门禁日志：[iteration-388-migration-gates.log](../build/optimization/iteration-388-migration-gates.log)，SHA256 `c296a0888f9803edd4fc0ecb5d5f67ab1b7089689ec0d222fbebbe46ba0493e3`。未连接读卡器或 SIM、未执行 IMS/运营商呼叫或短信、未生成 App/DMG。

P3 +0.025 计划人周等值，累计 **2.415 / 8.0 = 30.19%**；整体迁移累计 **16.895 / 47.5 = 35.5684%**（显示 **35.6%**，两位小数 **35.57%**，估算误差约 ±3 个百分点）。Go 仍拥有 SIP socket/事务、物理 AKA、IPsec 和 IMS session 生命周期；这只迁移 eap_direct 首次 REGISTER 授权计算，不等于 IMS/VoWiFi 完成。
