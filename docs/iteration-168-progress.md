# Iteration 168：N待恢复退避策略迁入 Rust

日期：2026-10-08。所有代码和验证只在 Switch 内完成。本轮未连接读卡器/SIM、访问运营商或真实 provider，未构建 App，也未生成 DMG。

将 N待自动恢复中的策略计算放到 Rust 私有接口 `POST /api/internal/multistandby/runtime/recovery-plan`：稳定在线满 5 分钟后清零连续故障计数；人工重试立即执行；计划续期/重认证采用 2 秒调度；意外失败按 2 秒递增并封顶的退避规则计算。请求只接受枚举原因和有界时间/计数，拒绝未知字段，并在内部认证、服务关闭和请求体限制下运行。

Go runtime 将有界计时事实提交给 Rust，再按返回结果安排 timer 并执行实际 Profile 会话恢复。Rust IPC 不可用或回复无效时，Go 暂时使用原策略作兼容回退。Go 仍拥有恢复 timer 的实际执行、Profile worker、runtime journal、IMS/VoWiFi 生命周期和物理卡调度；本轮迁移的是可验证的恢复策略决策，不代表 N待运行时已脱离 Go。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,459项通过、6项忽略**（daemon **962项通过、3项忽略**）；Go 全仓 `vet`/测试通过，2,722项载波回放、USB/radio/codec/HTTP-HEAD parity通过；Rust supervisor + Go fixture 的跨进程检查 **23项**通过；迁移契约 `--diff`、Rustfmt 与 `git diff --check` 通过。迁移清单 **296条**（Rust **232** / Go **64**，含 **38** 个 HEAD alias），127个 Swift HTTP 调用点、277个审阅范围。`--require-rust-only`仍失败：默认 Go fallback 尚存，150条 route 仍有 Go ownership/dependency。

保守计入 P4 **+0.025计划人周等值**，从0.525增至 **0.550/4.5（12.22%）**；总体从13.795增至 **13.820/47.5≈29.1%**（估算误差约±3个百分点）。Go N待 session/broker、多 Profile 实时并发、无 fallback 的 Go-free 构建/运行、实机与长稳验收仍未完成。
