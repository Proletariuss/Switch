# Iteration 135：通话模式只读状态判定迁入 Rust

本轮将 call-mode readiness 决策从 Go 移进 Rust，但范围严格限定为只读分类。新增私有认证且限长的 `POST /api/internal/call-mode/status/project`，输入 USB composition、IMS/VoLTE 配置、运行时完整性和阶段性 ADB 探测结果。Rust 严格验证输入，再输出 `ready`、`needs_voice`、`needs_usb`、授权/接口恢复、运行时下载或断开等状态、动作确认门禁及下一项只读探测选择。该接口不执行硬件 I/O、文件读写或设备更改。

Go 仍采集硬件和模块事实、公开状态 DTO/cache、协调状态阶段、执行被 Rust 指定的 ADB 只读探测，并持有启用/恢复/部署等动作。Rust 请求失败、超限、响应字段不一致或探测结果无法匹配状态机时 Go 失败关闭。通话呼入/呼出与 IMS/SIP/媒体没有在本轮迁移。

主要代码：[Rust 投影状态机](../rust/crates/proletariusd/src/call_mode_status.rs)、[Go 投影客户端](../backend/cmd/proletarius-macos/call_mode_status_projection.go)、[Go 状态流程接入](../backend/cmd/proletarius-macos/call_mode.go)。Rust internal listener 和 ownership/call-mode contract review 已登记该路由；动态源码位置已复核。

验证结果：

- Rust workspace：**818 passed、0 failed、3 ignored**。
- Go `cmd/proletarius-macos` 全包 race 测试、`go vet`：通过。
- Rustfmt、Go 格式、`git diff --check`：通过。
- Rust migration contract 工具：**42 项自测通过**；清单核对一致，275 条 method/path（Rust 194、Go 81，含 30 个 HEAD 别名）、127 个 Swift HTTP 调用点、222 个审阅范围。Rust-only 门禁仍失败，因为默认 Go fallback 与 Go 路由/业务依赖仍存在。
- Release `scripts/build.sh`：`Switch.app` 2.87/build287 arm64 成功，deep strict code signature 通过。应用内 `switchd-rs` SHA-256：`1b50f46f880b688c9dedc8f2d15c522b2c2dec922aeb4226dc3a30a1912e08da`。

没有连接读卡器、模块或实体 SIM，没有执行呼叫、写卡或短信，也未生成 DMG。用户明确表示号码未响铃按号码问题处理并停止排查；本轮没有相关测试。

保守计入 P2B **+0.025 计划人周**：4.450→**4.475/8.0（55.94%）**。累计：12.920→**12.945/47.5≈27.3%**（估算误差约 ±3 个百分点）。没有将公开状态缓存、观察来源、通话模式变更/恢复、硬件验收或零 Go 构建/运行门禁提前计入。
