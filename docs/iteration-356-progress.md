# Iteration 356：VoWiFi 短信文本编码迁入 Rust

本轮将读卡器和模块 VoWiFi 发信路径中的文本分段、TP-MR 引用号分配及 RP-DATA 组装迁入现有 Rust IMS SMS 事务 owner。Go 通过已认证的内部 UDS 动作提交号码、SMSC、文本和状态报告选项，验证返回的分段数量、引用号及 RPDU 边界，再交给原有 IMS `Service.SendSMS` 发送。SIP MESSAGE 头部/事务、当前注册会话、socket I/O、reader 身份二次核验和错误状态仍由原路径管理。

行为保持要求：Auto GSM-7/UCS-2 分段与原 Rust `proletarius-sms` 编码器一致；状态报告仍启用；随机 RP 引用种子由系统熵产生并在失败时拒绝发送；每段 TP-MR 使用递增且按 8 位回绕的引用值；SMSC/RPDU 仍使用同一 Rust 编码实现。生产 VoWiFi 发送不再在 Go 中调起 TPDU/RPDU 编码 RPC 或逐段拼装。

三轮交叉审议：

1. 报文兼容：对比原流程的 Auto 编码、状态报告位、RP-MR/TP-MR 和分段顺序；固定向量及多段引用回绕测试通过。
2. 安全与失败边界：随机源失败关闭；Rust 限制 1–255 段；Go 校验每个 RPDU 长度、消息类型、地址长度和 TPDU 长度，格式错误不会交给 SIP 发送。
3. 迁移与计分：本轮只计入生产 VoWiFi 文本/TPDU/RPDU 组装职责（P3 +0.025）；Go SIP session/socket、来信 RP-ACK 以及其他非 VoWiFi SMS codec 路径没有计为迁出。没有真实卡片、网络或短信验收。

验证：Rust workspace 1361 项通过、3 项忽略；Go 全包测试与 `go vet`、Swift BLE 物理完成证明测试、carrier/USB/radio/HTTP/IMS 音频与 MESSAGE parity 均通过。日志：[iteration-356-migration-gates.log](../build/optimization/iteration-356-migration-gates.log)。迁移契约扫描为 343 个 method/path、353 个审阅范围，inventory 与当前源码一致；`git diff --check` 通过。

本轮 P3 增加 0.025 计划人周等值，累计 **16.145 / 47.5 = 33.9895%（显示 34.0%，两位小数 33.99%）**，估值误差约 ±3 个百分点。全 Rust 后端目标尚未完成，不生成 DMG。
