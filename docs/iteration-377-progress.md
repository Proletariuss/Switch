# Iteration 377：REGISTER transport 事务头构造迁入 Rust

生产 REGISTER session 现在将 transport、local IP/port、Call-ID 与当前 CSeq 交给 Rust IMS codec。Rust 负责 IPv4/IPv6 Via host 格式、RFC3261 magic-cookie branch 熵、Via/CSeq/Max-Forwards 字段构造及输入校验；Go 保留会话 CSeq 计数、连接端口选择、把字段写入兼容 SIP 对象、TCP socket I/O 与 session 生命周期。生产路径配置 Rust builder 后，如果 Rust/熵源失败则终止请求构造。复用既有认证 IMS codec 操作，没有新增 HTTP route。

定向验证：`proletarius-ims` REGISTER transport headers 3 项测试，`proletariusd` bridge 1 项测试，Go `imscore` 全包测试与 `proletarius-macos` 全包测试通过；另外 Rust callback 失败关闭/IPv6 和 Go Unix socket bridge 有专门覆盖。

完整迁移门禁通过：[iteration-377-migration-gates.log](../build/optimization/iteration-377-migration-gates.log)。Rust workspace **1,937 passed / 6 ignored**（daemon **1,384 passed / 3 ignored**）；Go 全包测试与 vet、Swift BLE 与协议 parity、API 契约 diff 和审计器43项测试通过。契约仍为343 method/path（Rust 283 / Go 60）、127 Swift 调用点、356 审阅范围。

本轮计入 P3 **+0.025 计划人周等值**，累计 **2.165 / 8.0**；全项目 **16.645 / 47.5 = 35.0421%**，显示 **35.0%**（两位小数35.04%，估值误差约±3个百分点）。Go 仍负责 REGISTER 的 CSeq 计数与实际连接/会话生命周期、物理 AKA、IPsec 和 IMS 网络事务；本轮只是 transport 头格式及分支熵的生产责任迁移。没有操作实体读卡器、号卡、短信/呼叫或运营商网络，没有生成 DMG。
