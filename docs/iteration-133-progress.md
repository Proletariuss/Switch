# Iteration 133：Rust 接管模块语音路由生命周期

日期：2026-10-08。全 Rust 后端迁移继续进行；本轮未改变最终目标与 47.5 计划人周基线。

## 完成内容

模块 D4/UAC 语音路由的 `start`、`stop`、`status` 现在由 Rust 私有接口执行。Rust 端使用认证的内部路由、单飞 ADB 准入和共享 USB flock；每条设备命令创建独立 ADB/libusb 会话，以兼容 USB gadget 重枚举。停止操作仅在 helper PID、启动时间和完整 argv 均与 Switch 所有权匹配时发送 SIGTERM，并通过 readiness 检查确认结果。启动/停止结果不确定时，Go 适配器会回查 Rust 的实际路由状态；T/T/B 混音器失败会执行有界回滚。

Go 删除了本地 helper 生命周期与缓存的路由布尔值，改为调用带 producer generation 的 Rust UDS 适配器。Go 仍保留更上层的通话模式状态机、USB transition lease 和业务协调。reader VoWiFi USSD 路由仍由 Go 承担，并已在人工复核的迁移清单中明确登记。

## 验证与限制

- Rust 路由生命周期定向测试：32 项通过。
- Rust workspace：812 项通过、0 失败、3 项忽略。
- Go `cmd/proletarius-macos` race 测试与 `go vet` 通过。
- Rustfmt、`git diff --check`、迁移契约清单与差异检查通过；人工复核后的 census 为 273 条 method/path（Rust 192、Go 81，含 30 条 HEAD alias）、129 个 Swift HTTP 调用点、221 个审阅范围；契约扫描器自测 42 项通过。
- 完整 Rust-only 路由退出门禁仍失败：存在默认 Go fallback，且有 150 条 route 条件仍引用/依赖 Go。此门禁失败是当前剩余迁移工作的证据，不是可忽略的通过项。
- Release `scripts/build.sh` 成功，产物 `/Users/proletarius/Desktop/Switch/dist/Switch.app` 为 2.86/build286 arm64；deep strict 签名验证通过。构建仍嵌入 Go 和 Rust 后端，不是 Go-free 构建。完整命令状态见 [最终验证记录](../../build/optimization/iteration133/final-verification.md)。
- 未操作模块、读卡器、实体 SIM 或短信；未生成 DMG。

## 进度

本轮只为可离线验证的 D4/UAC route lifecycle 迁移计入 P2B **+0.075 计划人周**：P2B 从 4.350/8.0 增至 **4.425/8.0（55.31%）**；总体从 12.820 增至 **12.895/47.5 计划人周（27.1%）**，估算误差约 ±3 个百分点。未计入设备验收、Go 外层状态机退出或 UI 改动。

剩余边界包括 Go 通话模式状态机和 USB transition lease、ADB helper 部署/重启恢复、reader VoWiFi USSD、其他硬件/IMS/eSIM业务、EVS/AMR 端到端 Rust owner、Go-free 构建与真实设备验收。`9eSIM 6982` 仍按用户要求排除。
