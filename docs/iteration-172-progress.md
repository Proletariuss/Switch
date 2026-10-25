# Iteration 172：N 待维护调度与短信入队迁入 Rust

日期：2026-10-09。所有源码和构建验证均在 `/Users/proletarius/Desktop/Switch/` 内完成。

Rust daemon 现在持有 N 待每秒维护节拍；Go 原先每个 Profile 内的 ticker 已移除。Rust 只在 Go backend 就绪时通过认证内部接口发送合并后的维护信号，Go adapter 通过每个活动 session 的单槽 channel 合并积压信号。实际 outbox flush、运行状态采样、Profile session 和读卡器生命周期仍由 Go 执行。

`POST /api/multistandby/messages/send` 的公开处理和耐久入队已迁至 Rust：Rust 校验请求、生成与旧 Go `multiID` 相同的幂等事件 ID，并通过 Rust store 写入队列。Go 仅保留带内部凭证的只读 readiness adapter，核对当前读卡器代次、session 和短信通道；该路径只负责排队，不会自行发送短信。202 响应、请求幂等及既有就绪错误保持兼容。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace 测试与格式检查通过，`proletariusd` **971 项通过、3 项忽略**；Go 全仓 `vet` 和测试通过；2,722 项 carrier replay、USB 状态、radio、HTTP method/HEAD、codec/SDP parity 通过。Rust/Go 隔离进程集成 **28 项**通过，迁移契约 `--diff` 和 **43 项**工具自测通过。当前契约清单 **298 条**（Rust **233**、Go **65**，含 38 个 HEAD 别名），127 个 Swift HTTP 调用点、277 个审阅范围。Rust-only 退出门禁仍受默认 Go fallback 和剩余 Go 业务依赖阻断。

完整门禁首次发现冻结 Go HTTP 方法基准漏列本轮已迁移的发送路由；已将历史 `POST` 模式补入独立 oracle 并重跑完整门禁通过。`git diff --check` 与 Rustfmt 通过。没有操作实体读卡器、SIM、运营商网络，也未构建 App 或 DMG。

本轮保守计入 P4 **+0.050 计划人周等值**，至 **0.675/4.5（15.0%）**；总体 **13.945/47.5（29.4%）**，估算误差约 ±3 个百分点。尚未完成的核心工作包括 N 待 broker/session 与多 Profile 实时仲裁、硬件生命周期迁移、Go-free 构建运行和设备长稳验收。
