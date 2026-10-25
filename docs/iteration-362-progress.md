# Iteration 362：IMS AKA 摘要计算接入 Rust

本轮将 IMS `AKAv1-MD5` 摘要响应的计算接入 Rust daemon：IMSCORE 仍通过既有 SIM provider 执行一次 AKA，随后把原始 RES 或 AUTS、REGISTER 挑战字段及请求信息经本机认证 UDS 交给 Rust `proletarius-ims` 计算。Go 保留 SIP 状态机、qop 选择、外层凭据格式、AKA 卡操作和网络发送；主读卡器 VoWiFi 生产会话注入 Rust 回调。未接回调的兼容调用仍保留 Go 路径，不计作 Go 退出。

Rust 端校验算法、URI、qop、计数器与 RES/AUTS 长度；成功和错误路径都清理输入 secret 缓冲。桥接端校验摘要格式、userhash 与 AUTS 形状。测试覆盖独立 RFC 向量、原始 RES 字节、`auth` qop、同步失败 AUTS、userhash、Go 生产调用回调及 UDS 字节传输。

验证：Rust daemon 定向测试通过；Go `imscore` 与 macOS daemon 定向测试通过；完整 `scripts/test-migration-gates.sh` 通过（Rust daemon 1,368 passed / 3 ignored，Go 全包测试/vet、Swift BLE proof、协议 parity 均通过）；迁移契约扫描 diff 通过，343 method/path（Rust 283 / Go 60）、127 Swift 调用点、353 审阅范围。详见[完整门禁日志](../build/optimization/iteration-362-migration-gates.log)。

本轮 P3 +0.025 计划人周等值；累计 P3 **1.815 / 8.0 = 22.7%**，全项目 **16.295 / 47.5 = 34.3053%**（显示 34.3%，两位小数 34.31%；估值误差约 ±3 个百分点）。没有执行实机 SIM、短信或通话测试；没有生成 DMG。Go AKA 算法/卡访问兼容路径、SIP 构造与 transport/session 仍未退出。
