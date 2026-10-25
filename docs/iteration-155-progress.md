# Iteration 155：BLE eSIM 健康读取的 APDU 与 Profile 扫描迁入 Rust

## 完成内容

选中的 BLE 读卡器现在复用 Rust eSIM health/profile scanner。Rust 通过受认证、长度受限的 Unix socket 调用 Swift CoreBluetooth bridge；每条 APDU 都绑定预期 BLE reader identity，由 bridge 在访问 CoreBluetooth 前重新核验连接身份。Rust 顺序执行 APDU，不重试；扫描任务继续受既有 generation-bound reader-session lease 与 heartbeat 保护，租约失效后停止后续 APDU。

桥接器或配置尚未就绪且未发送 APDU时，保持旧 Go 兼容响应；一旦提交 APDU，失败就按原失败边界返回。候选 ISD-R AID 仅在明确返回 `6A82` 时跳过；`6982` 不重试、不绕过鉴权，并继续按用户要求排除 9eSIM 下载问题。

Go 仍负责 BLE reader 选择、会话准入/释放及完整扫描/连接/断开生命周期，Swift 继续承载 CoreBluetooth 系统桥。本轮没有把 BLE 端到端或读卡器硬件 owner 标记为 Go-free。

## 验证

- `cargo +1.98.0 fmt --all -- --check`：通过。
- Rust 全 workspace 离线测试：**1,417通过、0失败、6忽略**；`proletariusd` 为 **921通过、3忽略**。详细日志：`build/optimization/iteration-155/rust-workspace-tests.log`。
- 迁移契约 `--diff` 和 **43项**工具自测：通过。最新盘点为 **287** 条method/path（Rust **216**、Go **71**，含 **35** 个HEAD别名）、127个Swift HTTP调用点、254个审阅范围。
- `--require-rust-only` 仍按预期失败：默认Go fallback仍在，144条路由仍含Go owner/dependency。
- `Switch.xcodeproj` Debug 构建通过；产物位于 `build/call-layout-verification/Build/Products/Debug/Switch.app`。本次没有重跑Go全仓测试；BLE迁移没有改Go源码，最近一次完整Go测试与parity门禁记录在Iteration 154。

同轮修正了通话页呼入接听/挂断按钮组偏左：按钮组现在在通话卡片的完整可用宽度内居中。Debug构建验证通过；这项纯UI调整不计入Rust进度。

没有连接读卡器、操作实体SIM、发送短信、拨打电话或访问运营商网络；没有生成DMG。P2B计入 **+0.025计划人周**，由4.825升至**4.850/8.0（60.63%）**；整体由13.395升至**13.420/47.5≈28.3%**（估算误差约±3个百分点）。这不代表BLE实机验收、完整LPA迁移或最终Go退出。
