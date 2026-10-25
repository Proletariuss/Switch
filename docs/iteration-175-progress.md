# Iteration 175：公开健康状态与 N 待重连代次迁入 Rust

日期：2026-10-09。所有改动和生成文件均位于 `/Users/proletarius/Desktop/Switch/`。

Rust 网关现在直接生成 `/api/health` 的兼容响应，并以同一代次绑定的投影为 `device-snapshot` 提供健康字段。健康事实通过带内部认证的 Go observation adapter 获取；Rust 在调用前后验证 Go 子进程仍处于同一可用代次，并校验请求代次、响应 DTO 与必需字段。Go `GET /api/health` 在显式兼容启动模式中继续工作，因此没有移除 Go 健康采集或 Go 运行依赖。

N待重连调度也完成了一项所有权迁移：Go 不再分配 retry generation，只发送每次尝试的 opaque token 与有界 delay。Rust 按 Profile 原子分配并持久化单调 generation 和 due time；打开旧 journal 时从 pending schedule 播种计数器，保持向前递增。相同 token 的重复请求幂等，改动 delay 后复用 token 返回冲突。Rust 到期 callback 带 owner/generation/token，Go 只有当前期望 Profile 和 token 一致时才唤醒恢复；过期通知或人工重试后已失效的通知会被确认但不执行。Rust 负责时钟、generation 与栅栏，Go 仍实际重启 session/card，并保留 IPC 失败时的有界本地 timer。

完整 `scripts/test-migration-gates.sh` 通过；`proletariusd` **982 passed/3 ignored**，Go 全仓 `vet`/测试、2,722 项 carrier replay、USB/radio/HTTP method+HEAD/codec+SDP parity 通过。Rust retry journal 21 项定向测试与 30 项 Rust/Go 跨进程集成覆盖代次分配、旧 journal 恢复、重复/冲突请求和迟到 callback；健康投影测试覆盖 wire shape、必需字段、代次不匹配和查询期间子进程重启。新增健康合同为 `HEAD /api/health` 与私有 `POST /api/internal/runtime/health-snapshot`。迁移合同为 **304 条**（Rust 237、Go 67，含 39 个 HEAD 别名）、127 个 Swift 调用点、284 个审阅范围；清单校验、差异检查及 43 项检查器自测通过。

`--require-rust-only` 仍被默认 Go fallback 和现有 Go owner/dependency 阻断。没有操作读卡器、号卡、短信或运营商网络，也未生成 App/DMG。

P2A 健康投影保守计入 **+0.025 计划人周等值**，达到 **3.910/5.5（71.1%）**；P4 重连代次/调度迁移另计 **+0.025**，达到 **0.750/4.5（16.7%）**。总体达到 **14.045/47.5≈29.6%**（估算误差约 ±3 个百分点）。这些有限迁移不代表 Go-free 设备发现、N待物理/session 接管或最终运行时切换已完成。
