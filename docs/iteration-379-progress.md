# Iteration 379：REGISTER challenge sec-agree 决策迁入 Rust

日期：2026-10-12。将收到 401/407 后的 sec-agree 决策从 Go 生产策略接入 Rust：Rust 根据 Security-Server 是否存在、模板 sec-agree 状态及 Security-Client mechanism 数量决定 IPsec 安装要求、Security-Verify 要求或稳定错误。Go 继续控制 AKA/APDU、IPsec transform 安装、SIP 发送、REGISTER challenge 重试和 IMS 会话生命周期。该迁移沿用现有 IMS codec 认证 UDS，不新增公开路由；9eSIM `6982` 不在本轮范围。

保留既有策略和错误语义：sec-agree 关闭时忽略服务器 offer；auto 模式缺少 Security-Server 时禁用 sec-agree 并继续 plain Digest 路径；required 模式缺少 Security-Server 返回原错误；协商已启用但无 Security-Client mechanisms 返回原错误；有服务器 offer 和有效 mechanisms 才进入 IPsec/要求 Verify。Go 会核对 Rust 的 reason 与全部决策标志，不一致时失败关闭。

验证：Rust IMS 定向测试 **5 passed**；daemon codec 测试 **1 passed**；Go imscore、native bridge 和 macOS backend 定向测试通过。完整迁移门禁通过：Rust workspace **1,941 passed / 6 ignored**（daemon **1,386 passed / 3 ignored**），Go 全包 test/vet、Swift BLE 与协议 parity 检查通过。API census/diff：343 method/path（Rust 283 / Go 60）、127 Swift 调用点、356 审阅范围；契约扫描器 **43 tests passed**。日志 SHA256：`477338d96a42cf6485d5f696f06fec9ca41ff72f60b326efd82d565c1432efcb`。

本轮计入 P3 **+0.025 计划人周等值**，累计 **2.215 / 8.0**；全项目 **16.695 / 47.5 = 35.1474%**，显示 **35.1%**（两位小数 35.15%，估值误差约 ±3 个百分点）。未做实体读卡、短信/呼叫或运营商验收，也未生成 DMG。Go 仍掌握完整 IMS session FSM、物理 AKA、IPsec 执行、SIP socket/事务与呼叫生命周期。
