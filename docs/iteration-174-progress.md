# Iteration 174：IMS 注册续期 deadline 迁入 Rust

日期：2026-10-09。所有修改和验证均在 `/Users/proletarius/Desktop/Switch/` 内完成。

Rust runtime journal 新增独立于 N 待重连退避的 IMS REGISTER lease deadline。期限来自运营商授予的 registration lease；Rust 原样安排续期时刻，不加入重连抖动，避免挤占续期安全余量。每个 Profile/owner/session-generation 的 deadline 有界、可耐久重开，同一 generation 重复请求保持幂等，过期 callback 被确认或 fence 后耐久清除。

当 Rust scheduler 可用时，Go 不再启动 N 待 IMS 注册续期本地 timer。Rust dispatcher 只在 Go backend ready 时发送认证 callback；Go 核对 boot owner、Profile、desired/session 状态和 session generation 后，通过既有 failure arbitration 安排 planned renewal。Go 仍负责 SIP 注册、session teardown/reconnect、物理读卡器操作；Rust IPC 失败时 Go 保留有界 timer fallback。此项没有迁移 SIP socket、REGISTER 事务或 IKE/EAP 会话。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,474 passed / 6 ignored**（`proletariusd` **975/3**），Go 全仓 `vet`/测试、2,722 项 carrier replay、USB/radio/HTTP method+HEAD/codec+SDP parity。Rust/Go supervisor 隔离进程集成 **30 项**通过，包含重连调度和独立 IMS REGISTER 到期 callback；契约 `--diff` 与 **43 项**工具自测通过。当前 API census 为 **302 条**（Rust **235**、Go **67**，含 38 个 HEAD alias）、127 个 Swift HTTP 调用点、278 个审阅范围。`--require-rust-only` 仍因 Go fallback 与 Go 业务依赖未退出而阻断。未操作实体设备、SIM、短信或运营商网络，也未构建 App/DMG。

保守计入 P4 **+0.025 计划人周等值**，至 **0.725/4.5（16.1%）**；总体 **13.995/47.5≈29.5%**（估算误差约 ±3 个百分点）。N 待 broker/session、多 Profile 在线仲裁、物理硬件生命周期、Go-free 构建/运行及设备长稳验收仍未完成。
