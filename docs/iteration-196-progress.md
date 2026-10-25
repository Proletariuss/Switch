# Iteration 196：call-mode status 公共入口迁入 Rust

日期：2026-10-09。监督模式下 `GET /api/call-mode/status` 现在由 Rust 拥有公开路由、响应 schema 校验及错误映射。Rust 经带内部凭证的 UDS observation adapter 获取当前兼容快照；无效成功响应返回明确的 502，Go observer 不可用时返回可重试的 503。Rust 公共策略明确限制为 GET/HEAD，HEAD 继续复用 GET 的响应头语义并清空响应体。

Go 仍读取缓存的 call-mode 状态，并负责 USB/ADB/语音硬件事实、后台准备和恢复。因此本轮只是公开 gateway 边界迁移，未将 call-mode 状态链路端到端移出 Go。显式 Go 兼容启动保留原公开 handler。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,082 passed / 3 ignored**，Go 全仓 `go vet` 与 `go test` 通过，2,722 项载波模板回放及 USB 状态/radio/HTTP method+HEAD/SDP/AMR parity 均通过。迁移契约扫描器43项自测、清单 `--diff` 与 `git diff --check` 通过。清单328条（Rust264、Go64，含48个HEAD alias）、127个Swift HTTP调用点、298个审阅范围。`--require-rust-only` 按预期失败：默认Go fallback仍存在，173条路由仍有Go所有权或运行依赖。门禁日志：`build/reports/migration-gates-iteration196.log`。未访问实体读卡器/SIM或运营商网络，未构建App/DMG。

保守计入P2B **+0.025计划人周等值**；累计 **14.670/47.5≈30.9%**（估算误差约±3个百分点）。P2A **4.035/5.5（73.4%）**，P2B **5.375/8.0（67.2%）**，P3 **1.640/8.0（20.5%）**，P4 **0.750/4.5（16.7%）**。call-mode 状态底层观察、硬件生命周期及启动/自动恢复仍依赖Go。
