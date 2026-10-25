# Iteration 380：REGISTER challenge 重试策略迁入 Rust

日期：2026-10-12。主读卡器 REGISTER 认证循环现通过既有 authenticated IMS codec 调用 Rust，决定收到认证响应后是继续处理 challenge 还是停止。重复普通 MD5/空算法 challenge 立即停止；AKA challenge 最多六次。普通认证响应和 AUTS resynchronization 响应统一调用该策略。未新增 HTTP 路由。

生产 reader VoWiFi session 已配置 Rust evaluator；Go 继续执行 SIP 请求/响应、SIM AKA、IPsec 安装与 IMS 生命周期。Rust 请求或返回值不一致时失败关闭。未配置 callback 的旧测试/兼容 fixture 仍保留原行为，不属于生产 reader path。

定向验证：Rust IMS **3 passed**，daemon codec **1 passed**，Go imscore 与 macOS backend 定向测试通过。完整迁移门禁通过：[日志](../build/optimization/iteration-380-migration-gates.log)，Rust workspace **1,945 passed / 6 ignored**（daemon **1,387 passed / 3 ignored**），Go 全包 test/vet、Swift BLE 与现有协议差分通过。契约扫描器 **43 tests passed**；清单 **343 method/path（Rust 283 / Go 60）**、127 Swift 调用点、356 审阅范围；`--diff`、Rustfmt 和 `git diff --check` 通过。门禁日志 SHA256：`7364dee75662f6d1bfe144ba0e5e232a67711125b69810f3aa1ef37d156827a4`。

本轮没有进行实卡/运营商验收，没有发送短信或拨号，也没有生成 DMG。全 Rust 后端仍未完成：Go 保留 IMS FSM、TCP socket/事务、物理 AKA、IPsec 执行、呼叫生命周期及其他业务/硬件职责。

本轮计入 P3 **+0.025 计划人周等值**，累计 **2.240 / 8.0**；项目累计 **16.720 / 47.5 = 35.2%**（两位小数 **35.20%**，估值误差约 ±3 个百分点）。
