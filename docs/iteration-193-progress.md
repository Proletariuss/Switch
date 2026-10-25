# Iteration 193：模块拨号入口策略迁入 Rust

日期：2026-10-09。监督模式的 `POST /api/call/dial` 现在由 Rust 负责限长 JSON 解码、去空白、号码格式/长度校验、从实时通话状态确认 idle，以及返回兼容的 `call_not_idle` 错误。格式错误不会访问状态适配器；不空闲、状态未知或适配器不可用都不会进入拨号执行。通过 Rust 准入后只调用固定的认证 Go 内部拨号适配器。

Go 适配器在 ATD 边界再次核验号码，保留 call-mode/RF 检查、SIM 互斥、USB transaction lease、固定 Rust modem actor 呼叫命令和通话历史副作用。当前没有把 Rust 状态快照 revision 与 ATD 组成原子准入，状态仍可能在观察与执行之间改变；旧路径也将状态观察与ATD分开。显式 Go 兼容启动仍使用原公开路由。该切片只迁移公开输入与准入策略，实际呼叫生命周期、SIP/媒体及硬件边界仍依赖 Go/现有 actor。新增 fake UDS 测试覆盖有效号码转发、无效号码不触达硬件、非空闲和不可用状态 fail-closed；Go adapter 测试覆盖token和危险输入。

验证通过：完整 `scripts/test-migration-gates.sh`，Rust workspace 1,070 passed / 3 ignored，Go 全仓 `test`/`vet`、2,722 项载波回放、USB 状态/radio/HTTP method+HEAD/codec-SDP parity；迁移契约工具43项测试、独立30项真实Go/Rust进程集成检查及 `git diff --check`通过。契约清单323条（Rust258、Go65，含46个HEAD alias）、127个Swift HTTP调用点、296个审阅范围。`--require-rust-only`仍按预期失败：默认Go fallback存在，168条路由有Go owner或依赖。

保守计入P2B +0.025计划人周；累计14.595/47.5≈30.7%。P2A 4.035/5.5（73.4%），P2B 5.300/8.0（66.3%），P3 1.640/8.0（20.5%），P4 0.750/4.5（16.7%）。未访问实体读卡器、SIM或运营商网络，没有拨打电话，也未生成DMG。
