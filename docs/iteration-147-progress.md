# Iteration 147：BLE 状态只读接口迁入 Rust

日期：2026-10-08。所有源码及验证材料均留在 `/Users/proletarius/Desktop/Switch/`；9eSIM `6982`不在本轮范围内，不生成 DMG。

`GET /api/reader/ble/status` 的公开路由及响应投影已从 Go 移入 Rust。Rust 在既有应用令牌认证后，通过受令牌保护的 Unix socket 调用 Swift CoreBluetooth 桥的只读 `status` 动作；返回原有 `devices`、`connected`、`state`、`error_code` 字段，并保留 `ble_bridge_unavailable`、HTTP 503 与 `recoverable:true` 错误形状。连接期限为3秒、整体期限为5秒，桥接响应上限与 Go 的 160,000 字节读取限制一致（包括换行）。缺少设备字段时仍投影为空字符串，保持 Go 解码器的零值行为。

Go 侧公开 status handler 和 mux registration 已删除；冻结的 Go ServeMux 夹具保留为方法/HEAD 协议 oracle。范围仅限公开只读状态接口：Go 读卡会话仍通过原 BLE 私有客户端读取状态、校验设备身份并传输 APDU；scan/connect/disconnect 也仍由 Go 执行。Swift CoreBluetooth/GATT 继续作为系统桥。因此这次不表示 BLE reader actor、APDU 或整个读卡器业务已迁移，也没有触碰实体读卡器或 SIM。

Rust BLE 状态单测 **7通过**，覆盖成功投影、bridge拒绝、未配置、设备字段缺省、最大与超限响应帧及旧错误结构；Rust GET/HEAD ownership 和 POST `Allow: GET, HEAD` 检查通过。全迁移门禁：Rust workspace **863通过、0失败、3忽略**；Go 全仓 `go test ./...`、`go vet ./...`，carrier/USB status/radio policy/HTTP-HEAD/PCMA fixture/SDP/AMR parity 通过。契约工具42项自测通过；刷新后的清单为 **279条 method/path（Rust205、Go74，33条HEAD alias）**、127个 Swift HTTP 调用点、228个审阅范围；scanner `--diff` 一致。

Rust-only 路由门禁仍失败：默认 Go fallback 尚在，139条路由合同仍有 Go owner/dependency。它仍是未完成项，不降低全项目目标，也不代表全部功能能在无 Go 环境运行。

Release `Switch.app` **2.93/build293 arm64** 构建成功并通过 `codesign --verify --deep --strict`；内嵌 `switchd-rs` SHA-256：`ddd300f398dca4212fb6bccb6480c5cc229cb417fbe0280bb368ffbf669cee82`。来电按钮布局修正包含在此产物中，但不计 Rust 迁移进度。没有访问真实模块、读卡器、SIM、通话、短信或第三方网络，也没有生成 DMG。

日志：[`migration-gates.log`](../../build/optimization/iteration-147/migration-gates.log)、[`release-build.log`](../../build/optimization/iteration-147/release-build.log)、[`contract-tests.log`](../../build/optimization/iteration-147/contract-tests.log)、[`contract-diff.log`](../../build/optimization/iteration-147/contract-diff.log)、[`contract-inventory-refresh.log`](../../build/optimization/iteration-147/contract-inventory-refresh.log)、[`rust-only-gate.log`](../../build/optimization/iteration-147/rust-only-gate.log)。

本轮保守计入 P2B **+0.025计划人周等值**，由4.700增至**4.725/8.0（59.06%）**；总量由13.195增至**13.220/47.5≈27.8%**（估算误差约±3个百分点）。此分数仅反映一个公开 BLE 状态路由及其契约退出，不计 BLE 设备 owner、Go 私有 bridge/APDU、实机行为或 Go-free 构建/运行。剩余迁移继续按项目目标推进。
