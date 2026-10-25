# Iteration 361：REGISTER 响应动作分类迁入 Rust

Rust 现有 REGISTER 响应验证器在解析完整 SIP 帧并核对 Call-ID、CSeq、方法后，返回 Rust 计算的 `accepted`（200）、`challenge`（401/407）或 `rejected` 动作。Go 注册 FSM 使用该动作选择成功收尾、继续 AKA 挑战，或进入原有拒绝/模板回退分支。AKA 计算、鉴权头、Security-Server 处理、SIP stream/socket、重试调度与网络生命周期仍由 Go 负责。Rust socket 未配置时保留原有兼容路径；它不算 Go 依赖已退出。

定向 Rust 分类测试、Go `imscore` 与 macOS adapter 测试通过。完整离线迁移门禁通过，日志见 [iteration-361-migration-gates.log](../build/optimization/iteration-361-migration-gates.log)：Rust workspace **1,367 passed / 3 ignored**，Go 全包测试/`go vet`、Swift BLE operation-proof/manager 回归、既有跨语言 parity 通过。契约 diff 通过：343 method/path（Rust 283、Go 60）、127 Swift 调用点、353 审阅范围，接口计数未增加。未连接实体卡、未发短信或拨号、未做运营商验收，也未生成 DMG。

本轮 P3 增加 **0.025 计划人周等值**，累计 **1.790 / 8.0**；全项目累计 **16.270 / 47.5 = 34.2526%**（显示 **34.3%**，两位小数 **34.25%**，估值误差约 ±3 个百分点）。详细工作包口径见[进度复核账本](optimization/all-rust/progress-reassessment-2026-10-10.md)。
