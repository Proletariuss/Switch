# Iteration 134：模块语音运行时就绪缓存迁入 Rust

本轮把 Go `moduleVoicePrepared` 布尔缓存迁入 Rust。Go 只保留受 `moduleVoiceMu` 保护的运行时代次标识，并通过内部 UDS 请求 Rust 部署或查询状态。Rust 的就绪缓存同时绑定 Go 后端 producer ID、runtime generation 和模块 `/proc/sys/kernel/random/boot_id`。状态查询读取模块当前 boot ID；模块重启或 Rust daemon 重启后不会沿用旧就绪结果。旧 generation、producer 变化、模块 boot ID 变化和状态不可确认都会失败关闭。

Rust 部署只有在本地工件验证、root/kernel 检查、ADB 同步传输、模块侧语音准备完成及 producer 仍有效后才提交 `prepared=true`。Go 部署客户端要求 HTTP 成功、`ready=true`、准确状态码和 `prepared=true`；缺少该字段或返回 false 均拒绝。状态查询失败时通话模式继续显示准备中并返回错误，不会用第二轮重叠部署掩盖状态不确定。

主要代码位于 [call_mode_adb.rs](../rust/crates/proletariusd/src/call_mode_adb.rs)、[call_mode.go](../backend/cmd/proletarius-macos/call_mode.go)、[module_voice_darwin.go](../backend/cmd/proletarius-macos/module_voice_darwin.go) 和 [call_mode_adb_client.go](../backend/cmd/proletarius-macos/call_mode_adb_client.go)。私有路由已加入 Rust authenticated dispatch、shutdown 策略及 ownership 清单；动态路由审阅记录已登记，API census 为 274 条 method/path（Rust 193、Go 81，含 30 条 HEAD 别名）、129 个 Swift HTTP 调用点、221 个审阅范围。

验证结果：Rust workspace **814 通过、0 失败、3 忽略**；Go `cmd/proletarius-macos` 全包 `-race` 测试和 `go vet` 通过；迁移契约工具 **42 项自测通过**，普通 census 一致性通过。Rust-only 检查仍按预期失败：默认 Go fallback 尚在，150 条路由仍有 Go owner 或依赖。Release `Switch.app` 为 **2.86/build 286 arm64**，deep strict 签名验证通过；Rust 可执行文件在构建产物与交付目录中 SHA-256 一致。日志见 [`Switch/build/optimization/iteration134/`](../../build/optimization/iteration134/)；本轮未生成 DMG。

没有连接或操作读卡器、模块、实体 SIM，也没有执行通话、eSIM 写入或短信。未完成 Go 外层通话模式状态机、transition lease 和恢复逻辑迁移；未完成零 Go 构建/运行依赖或设备验收。

本轮按可离线验证的 owner 边界收敛计入 P2B **+0.025 计划人周**，到 **4.450/8.0（55.63%）**；总体 **12.920/47.5≈27.2%**，估算误差约 ±3 个百分点。该进度不代表硬件或端到端通话验收完成。
