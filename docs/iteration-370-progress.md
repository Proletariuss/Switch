# Iteration 370：REGISTER Security-Client 头构造迁入 Rust

读卡器 VoWiFi 主注册路径现通过现有 IMS codec Unix bridge，由 Rust 按配置机制顺序生成 `Security-Client` header，保留 Go 原有参数顺序、空加密算法映射为 `null`、协议/模式字段规则与空格格式。Rust bridge 约束机制数量、字段长度、SPI、端口和返回头长度，并拒绝 CR/LF/NUL 注入；生产 REGISTER 构造传递活动上下文，取消/超时或 Rust 请求失败时不发送不完整注册请求。未配置 callback 的兼容入口仍保留 Go builder。

此轮只迁移 header 格式化，不代表 Rust 已负责 SIP parser/socket、物理 AKA、IPsec transform、事务/重试或 IMS 生命周期；这些仍在 Go。没有真实读卡、SIM、运营商、短信、通话或 DMG 验收。

定向验证：Rust formatter 精确 legacy-order/spacing 单测、daemon 既有 IMS codec operation 测试、Go Unix bridge 参数/响应测试、imscore callback/context/cancel 回归通过。完整迁移门禁：[iteration-370-migration-gates.log](../build/optimization/iteration-370-migration-gates.log) 通过：Rust workspace **1,376 passed / 3 ignored**（daemon **1,376 passed / 3 ignored**），Go 全包 test/vet、Swift BLE proof/manager regressions、carrier **2,722** cases、USB/radio/HTTP/HEAD/codec/MESSAGE parity、契约 inventory/diff、`cargo fmt --all --check` 与 `git diff --check` 通过。

P3 +0.025，累计 **1.990 / 8.0 = 24.875%**；项目累计 **16.470 / 47.5 = 34.6737%**（显示 **34.7%**，两位小数 **34.67%**，估算误差约 ±3 个百分点）。Go 仍承担 IMS/SIP 在线生命周期与物理卡/网络执行。详见 [Iteration 369](iteration-369-progress.md) 与 [进度复核账本](optimization/all-rust/progress-reassessment-2026-10-10.md)。
