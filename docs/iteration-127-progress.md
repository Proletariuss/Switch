# Iteration 127 — 通话模式 USB 目标配置生成迁入 Rust

日期：2026-10-08。

此前 Go 的 `callModeTarget()` 会复制当前 USBCFG、保留 VID/PID 与其他功能位，并开启 ADB/UAC。本轮把这项通话模式策略移至 Rust 的既有 `call_mode_usb_composition_read` typed action：Rust 在一次已验证读取中同时返回当前配置与目标配置，只改变最后两个 ADB/UAC 位。Go 只校验 typed target 的结构、身份、保留位和启用位，再将它交给既有 compare-and-write；还原流程仍使用通过 Rust 校验的备份原值，不受 enable target 影响。

验证：Rust workspace **767通过、0失败、3忽略**；Go `go test ./...` 全包、`cmd/proletarius-macos` 与 `internal/modem` race 测试及 `go vet` 通过；Python 脚本49项通过，迁移契约工具42项通过，264条契约扫描通过（Rust184 / Go80、127个Swift调用点、210个审阅范围）；Rustfmt 和 `git diff --check` 通过。未连接读卡器或实体卡。Release `Switch.app` 2.86/build286 arm64 构建成功，deep strict 签名验证通过；未生成 DMG。

P2B 计 **+0.025计划人周**，由 3.950 升至 **3.975/8.0（49.69%）**；整体由 12.420 升至 **12.445/47.5≈26.2%**（估算误差约 ±3 个百分点）。Go ADB/helper、通话模式状态机与恢复、其他硬件/IMS业务、Go-free构建和设备验收仍未完成。
