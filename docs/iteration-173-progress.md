# Iteration 173：N 待逐 Profile 耐久重试时钟迁入 Rust

日期：2026-10-09。所有修改和验证均在 `/Users/proletarius/Desktop/Switch/` 内完成。

Rust runtime journal 持久保存每个 Profile 的 reconnect backoff deadline、Go boot owner 与 attempt generation，限制状态容量和最大延迟。重复请求不会重算或延后已保存的 deadline；旧代次和冲突请求被拒绝，到期后只有收到认证 Go adapter 的确认才清除耐久状态。Rust dispatcher 只在 Go backend ready 时工作，并按 Profile ID 与 generation 稳定加入 0–999ms 抖动，避免多个 Profile 同时故障后同步重连。

正常路径不再由 Go per-Profile timer 唤醒重连 worker。Go adapter 检查 owner、Profile、desired 状态和 attempt generation；停止或手动重试会立即 fence 旧代次，迟到通知不能启动旧 session。Rust 调度 IPC 不可用时，Go 仍保留有界本地 timer fallback；实际 VoWiFi/IMS session 和读卡器生命周期由 Go 执行。

`cargo +1.98.0 fmt --all -- --check`、Rust workspace tests、Go 全仓 `vet`/测试、2,722 项 carrier replay、USB/radio/HTTP method+HEAD/codec+SDP parity、迁移契约 census 和 43 项工具自测均通过。Rust/Go 隔离进程集成 **29 项**通过，新增覆盖耐久重试 deadline、journal 重开、generation fence 和认证到期 callback。该轮契约清单为 **300 条**（Rust **234**、Go **66**，含 38 个 HEAD alias），127 个 Swift HTTP 调用点、277 个审阅范围。未操作实体读卡器、SIM、短信或运营商网络，也未构建 App/DMG。

保守计入 P4 **+0.025 计划人周等值**，至 **0.700/4.5（15.6%）**；累计 **13.970/47.5≈29.4%**（估算误差约 ±3 个百分点）。N 待 broker/session、多 Profile 在线仲裁、物理硬件生命周期、Go-free 构建/运行与设备长稳验收仍未完成。
