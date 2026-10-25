# Iteration 465：模块语音路由 USB AT 租约归 Rust owner

模块语音路由的 ADB/helper 启停本来已由 Rust 执行，但在 Go 侧仍由 `beginNativeUSBVoiceTransition` 持有 `purpose=voice` USB AT 租约。现在将租约的申请、旧句柄撤销、SIM/AT 并发阻止及释放统一收归 Rust `usb_at::Owner`。Rust voice-route start/stop 在 helper 操作前取得 producer/session/generation 绑定租约，等物理 worker 结束后确认释放；HTTP 客户端断开不取消后台清理。状态查询不申请租约。Go 删除对应字段和 lease RPC 包装，仍保留 `voiceUSBTransition` 用于串行化尚未迁移的 Go 消费者。

离线 fake-owner 回归确认：旧 AT 句柄先撤销，Voice lease 持有期间 SIM lease 被拒绝，释放后 SIM lease 可重新取得且会话代次变化。错误释放由 guard 尝试异步清理并返回未确认错误；该回退不代替正常路径的显式释放确认。

验证：`cargo +1.98.0 test --locked -p proletariusd` 全 crate **1,467 passed / 0 failed / 3 ignored**（覆盖 USB AT 71 项及 call-mode ADB 29 项）；`cargo +1.98.0 fmt --all -- --check` 通过；`go test ./cmd/proletarius-macos` 通过；`git diff --check` 通过。迁移合同清单刷新后 `--diff` 通过：354 method/path（Rust 293、Go 61，含 51 HEAD alias）、127 Swift HTTP 调用、379 review scopes；扫描器自测 **45 passed**。Go 退出审计仍有 6 类阻塞、266 个生产 Go 文件。

本轮仅为 USB 语音切换资源的一个 owner 子切片，不满足计划中的 P2B 或语音端到端计分点；总进度保持 **37.99%（18.045/47.5，约±3个百分点）**。公开通话状态机、硬件事实采集、来去电/媒体/SIP、模块配置备份恢复及其他 Go USB AT 消费者仍未迁完。测试使用离线模拟，没有操作 USB/ADB 设备、实体号卡或真实通话，不声称硬件行为已通过。当前源码尚未打入 DMG。

测试日志：[`iteration-465-proletariusd-tests.log`](../build/reports/iteration-465-proletariusd-tests.log)；合同检查日志：[`iteration-465-contract-check.log`](../build/reports/iteration-465-contract-check.log)、[`iteration-465-contract-selftest.log`](../build/reports/iteration-465-contract-selftest.log)。
