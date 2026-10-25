# Iteration 409：USSD SIP 请求线级编码迁入 Rust

生产读卡器 IMS 的 USSD `INVITE/INFO/BYE/CANCEL` 请求仍由 Go 决定 method、URI、有序 header 与 body，但请求 wire 编码现通过既有认证 IMS codec endpoint 交给 Rust；Rust 校验方法、目标、核心事务头、重复头、行/消息边界，补充准确 `Content-Length` 并安全处理二进制 body。Go 仍负责 USSD dialog、Call-ID/CSeq/branch 决策、写队列、SIP socket、响应派发和会话生命周期。未新增 HTTP 路由或任何卡片/网络 I/O。

Rust SIP serializer 与 Go sipgo legacy 字节回放一致；覆盖 binary body、header顺序、非法注入、重复核心字段和 CSeq 不匹配；Go 测试确认生产回调被调用，Rust 不可用时生产路径失败关闭。完整迁移门禁及契约 diff 通过：Rust workspace、Go 全包测试/vet、Swift BLE regression、carrier 2,722 例 replay 与既有 protocol parity 均通过。契约为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、363 reviewed scopes。日志：[iteration-409-migration-gates.log](../build/optimization/iteration-409-migration-gates.log)，SHA-256 `ad683d657e2776297756eb01e7a625b95e1cc4603c1e4c3fa77f53b4ff4be755`。

P3 +0.025，累计 **2.715 / 8.0**；全项目 **17.420 / 47.5 = 36.6737%（显示 36.67%）**。无 IMS/运营商实机验收。用户随后要求生成 DMG；3.08/build308 包含本轮改动，但不包含 Iteration 410。
