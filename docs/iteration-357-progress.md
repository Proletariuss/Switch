# Iteration 357：VoWiFi OPTIONS 心跳请求构造迁入 Rust

本轮将 VoWiFi OPTIONS 心跳的 SIP 请求报文构造从 Go 生产路径迁入 Rust。Go 通过现有已认证的 IMS codec UDS 路由提交 P-CSCF 目标、受保护流本地地址、身份、Security-Verify、Call-ID 和 CSeq；Rust 验证字段并生成完整 SIP/2.0/TCP 报文。Go 校验返回的会话/事务摘要、长度、base64、SIP 方法、Call-ID 和 CSeq 后，继续使用原有受保护 IMS stream 发送。

本轮保留现有注册会话、OPTIONS transaction lease、SIP stream 写入、精确响应等待、lease 完成/取消及 Rust heartbeat supervisor 的决定边界。随机分支与 From tag 使用系统熵源；熵失败、无效身份或无效路由均失败关闭。Rust 测试解析实际输出帧并检查 OPTIONS、Call-ID/CSeq、User-Agent、Security-Verify、IPv6 Via 和空消息体。

验证：Rust 定向测试通过；完整迁移门禁通过（Rust workspace 1,362 passed / 3 ignored；Go `go vet ./...` 与 `go test ./...`；BLE reader proof、38 组 BLE manager 回归及 carrier、USB、radio、HTTP、codec、REGISTERED MESSAGE 差分）。契约仍为 343 method/path（Rust 283 / Go 60）、127 个 Swift 调用点及 353 个审阅范围。门禁日志：[iteration-357-migration-gates.log](../build/optimization/iteration-357-migration-gates.log)。

P3 增加 0.025 计划人周等值，累计 **16.170 / 47.5 = 34.0421%（显示 34.0%，两位小数 34.04%）**。该估值只代表生产请求构造职责接管；IMS/IKE 会话、VoWiFi 硬件生命周期、短信/通话端到端和 P5 语音仍未完成。本轮未访问读卡器、未发送短信或拨号，也未生成 DMG。
