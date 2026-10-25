# Iteration 378：初始 REGISTER 安全策略迁入 Rust

日期：2026-10-12。将初始 REGISTER 收到 SIP 200 后的安全策略判断迁入 `proletarius-ims`：根据 SIP 状态、Security-Server 是否存在及模板 sec-agree enabled/required，决定进入 IPsec、允许明文继续或以 required 缺失错误失败。生产 Go IMS session 通过现有认证 IMS codec 路由调用 Rust，并校验结果；Go 仍安装 IPsec transform、执行 SIP 会话状态机及网络副作用。9eSIM `6982` 不在本轮范围内。

策略保留四种既有结果：有 Security-Server 且启用 sec-agree 时要求安装 IPsec；服务端有安全头但模板未启用时忽略协商并允许旧行为；auto 模式缺少安全头时明文继续；无安全协商时维持 plain 注册。required 缺头仍返回既有错误语义。Rust 决策请求失败、返回字段不一致或 required 缺头时均失败关闭，不能默默降级。

验证：Rust IMS 定向单测 **4 passed**；daemon codec 测试 **1 passed**；Go imscore 与 macOS backend 定向测试通过。全量迁移门禁通过：Rust workspace **1,939 passed / 6 ignored**（daemon **1,385 passed / 3 ignored**），Go 全包 test/vet、Swift BLE 与协议 parity 检查通过。API census/diff 通过：343 method/path（Rust 283 / Go 60）、127 Swift 调用点、356 审阅范围；契约扫描器 **43 tests passed**。日志 SHA256：`720920356c7256be9672a6b81c6e978ad35cc7eb7305745f414bd39b9967c706`。

本轮计入 P3 **+0.025 计划人周等值**，累计 **2.190 / 8.0**；全项目 **16.670 / 47.5 = 35.0947%**，显示 **35.1%**（两位小数 35.09%，估值误差约 ±3 个百分点）。本轮无读卡器、实体卡、短信、呼叫或运营商验收，也未生成 DMG。剩余的 Go REGISTER/IMS 网络事务、物理 AKA、IPsec 执行、业务生命周期和完整零 Go 构建/发布验收均未完成。
