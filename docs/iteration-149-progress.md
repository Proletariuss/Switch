# Iteration 149：读卡器 eSIM 健康查询迁入 Rust

日期：2026-10-08。源码和构建材料均位于 `/Users/proletarius/Desktop/Switch/`。本轮完成离线实现与集成构建；未做实体读卡器验收，不生成 DMG。

选定且已插卡的 USB/PC/SC reader 调用 `GET /api/reader/esim/health` 时，Rust 现在获取会话上下文并直接读取 eSIM profile 健康信息，保留原有公开 JSON 与当前操作投影。PC/SC 事务运行在有界阻塞 worker；HTTP 客户端断开后，worker 仍持有 reader lease，直到事务结束并确认释放，防止会话提前归还。

范围仍有限：reader manager 的选择状态、generation admission/release 和完整 operation 投影仍由 Go 提供；BLE、未选择 PC/SC、上下文取得失败及特定回退仍走 Go 兼容路径。已知 ISD-R AID 返回 `6A82` 时保留原分类回退；其他 APDU/传输错误失败关闭，尤其 `6982` 不重试、不绕过鉴权，9eSIM `6982` 仍排除在目标范围外。

同一合并切片还加固了模块 Profile 备注写入：不确定的写入结果会继续阻止重复写入，直到成功读取列表核对；错误响应不再回显完整 AT 记录。该修复没有额外计入迁移进度。

验证结果：Rust workspace **1,357 通过、0 失败、14 忽略**；Go 全仓 `go test -timeout=30s ./...` 与 `go vet ./...` 通过；冻结 Go HTTP GET/HEAD parity 测试通过；Rust 格式检查通过；迁移契约工具 **43 项**通过，普通扫描和 `--diff` 均通过。契约清单为 **280 条 method/path（Rust 207、Go 73，含 34 个 HEAD alias）**、127 个 Swift HTTP 调用点、237 个审阅范围。

Rust-only 退出门禁仍失败：默认 Go fallback 存在，另有 **142 条**路由带 Go 所有权或依赖。Rust Release 应用 `Switch.app` **2.93/build293 arm64** 构建并通过 deep/strict 签名验证；内嵌 `switchd-rs` SHA-256 为 `0157ca248b6ff52c8b4b188c7198a85263f16238340fe7cf910ef6a23381cee6`。本轮未操作 SIM、读卡器、通话或短信，也未生成 DMG。

按 47.5 人周基线，P2B 计入本次公开健康读取接管 **+0.025 人周等值**，由 4.725 增至 **4.750/8.0（59.38%）**；整体由 13.220 增至 **13.245/47.5≈27.9%**（估算误差约 ±3 个百分点）。该进度仅对应已迁移的 USB/PC/SC 健康读取路径，不代表 reader manager、BLE、完整 LPA、设备行为或 Go-free 运行完成。

验证日志：[`rust-workspace-tests.log`](../../build/optimization/iteration-149/rust-workspace-tests.log)、[`go-tests.log`](../../build/optimization/iteration-149/go-tests.log)、[`go-vet.log`](../../build/optimization/iteration-149/go-vet.log)、[`contract-tool-tests.log`](../../build/optimization/iteration-149/contract-tool-tests.log)、[`contract-scan.log`](../../build/optimization/iteration-149/contract-scan.log)、[`contract-diff.log`](../../build/optimization/iteration-149/contract-diff.log)、[`rust-only-gate.log`](../../build/optimization/iteration-149/rust-only-gate.log)、[`release-build.log`](../../build/optimization/iteration-149/release-build.log)。
