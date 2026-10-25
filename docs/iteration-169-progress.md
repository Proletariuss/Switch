# Iteration 169：N 待恢复策略状态持久化归 Rust

日期：2026-10-08。项目源码与本轮构建、测试材料均位于 `/Users/proletarius/Desktop/Switch/`。

本轮把 N 待恢复策略的每 Profile 失败次数和稳定在线累计时长从 Go 的本地影子状态移入 Rust runtime journal。恢复策略请求现在只传 Profile ID、受限原因码、Go 测得的在线时长和观测缺口标记；Go 不再把自己的故障计数或稳定时长当作 Rust 的策略输入。Rust 在响应前持久化新状态，并返回规范化后的原因码；Go 校验原因与策略响应一致。

旧 runtime journal 可继续读取：`recovery_states` 是可选字段，缺失时按空状态加载。状态按 Profile ID 隔离并设有 2,048 条上限；超限或持久化失败时拒绝生成策略结果。启动/人工停止会清除该 Profile 的失败预算，后端进程重启保留失败次数但清空无法可信延续的稳定在线累计时长。原因码只接受有界安全字符，未知原因映射为 `startup_failed`，原始运营商错误文本不进入 journal 或策略回包。

Go 仍保留 IPC 失败时的有界兼容策略回退，也仍运行恢复 timer、Profile 会话和实际读卡器操作。故障计数策略状态归 Rust 持有不代表 N 待的 session/broker、计时器、物理卡恢复或多 Profile 并发已迁移完成。

验证结果：Rust 定向 runtime-report 测试、私有恢复策略端点测试及 Go 恢复策略测试通过；真实监督进程 Go↔Rust 集成 **23 项全部通过**。`scripts/test-migration-gates.sh` 完整通过：Rust workspace **1,460 项通过、6 项忽略**，其中 daemon **963 项通过、3 项忽略**；Go 全仓 test/vet、2,722 项 carrier replay、USB/radio/codec/HTTP-HEAD parity 通过。迁移契约清单仍为 **296 条**（Rust 232、Go 64，含 38 个 HEAD alias）、127 个 Swift HTTP 调用点和 277 个审阅范围；契约扫描 `--diff` 与 `git diff --check` 通过。Rust-only 门禁仍因默认 Go fallback 和 150 条 Go owner/dependency 路由而不能通过。

保守计入 P4 **+0.025 计划人周等值**，由 0.550 增至 **0.575/4.5（12.78%）**；总量由 13.820 增至 **13.845/47.5（29.1%）**，约 29.147%，估算误差约 ±3 个百分点。未连接实体读卡器或 SIM，未做运营商/长稳验收，未构建 App 或生成 DMG。
