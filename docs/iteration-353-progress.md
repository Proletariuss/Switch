# Iteration 353：N待自动重连到期触发收归 Rust

日期：2026-10-11。工作目录：`/Users/proletarius/Desktop/Switch/src`。本轮未操作实体号卡/短信，也未生成 DMG。

## 实现

Iteration 352 已移除 Go 侧退避策略副本，但 Go 在 Rust retry-schedule RPC 失败时仍创建 `time.Timer`，并可自行唤醒 Profile。现在 N待自动重连只由 Rust durable scheduler 的到期回调触发；Go 根据 Rust attempt ID 与 generation 核验通知。移除了本地 timer 和 timer adapter。若 Rust 无法持久化/确认 deadline，Profile 保持降级可见，等待用户手动重试或停止，不猜测一个本地到期时间。

## 验证

- 定向测试覆盖 Rust durable schedule 成功、RPC 失败不被视作已调度、手动重试释放等待、停止不能再次重试，以及旧 attempt 不唤醒新代。
- `go test ./...` 与 `go vet ./...` 通过。
- `scripts/test-migration-gates.sh` 完整通过，含 Rust workspace、Go、Swift BLE 与 carrier/USB/IMS/codec/HTTP parity；日志：[iteration-353-migration-gates.log](../build/optimization/iteration-353-migration-gates.log)。
- 契约 census 仍为 343 method/path（Rust 283 / Go 60）、127 Swift 调用点、353 审阅范围；未新增路由。迁移契约 scanner 及 43 项工具测试通过。

## 当前边界与进度

P4 计入 +0.025 计划人周等值，累计 16.070 / 47.5 = 33.8316%（显示 33.8%，两位小数 33.83%）。Rust 独占恢复决策和持久 deadline；Go 仍接收 durable 到期通知并执行 N待 session 生命周期、SIP/IMS 与硬件会话。无真实 N待长稳验收，全 Rust 目标仍未完成。
