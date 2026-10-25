# Iteration 352：N待失败恢复策略收归 Rust

日期：2026-10-11。工作目录：`/Users/proletarius/Desktop/Switch/src`。本轮未操作实体号卡/短信，也未生成 DMG。

## 实现

Rust 的 N待运行日志已经持有稳定在线时长、失败次数和恢复退避决策。Go 侧仍保留同一退避算法，并在策略 RPC 失败或响应不合规时静默使用 Go 结果，造成双重策略 owner。本轮删除 Go 的退避实现与 fallback，恢复请求只向 Rust 提交 Profile、受限原因码、在线时间片和观测间隔。

如果 Rust 计划缺失或校验失败，Profile 保持 `degraded/backoff` 可见状态，自动恢复暂停并等待用户手动重试或停止；不会再绕过耐久策略本地重连。正常计划仍使用 Rust 返回的重试延迟、累计失败数和已持久化调度。

## 验证

- 定向 Go recovery-plan、Rust RPC 缺失/非法结果、N待观察与重试时序测试通过。
- `go test ./...`、`go vet ./...` 通过。
- `scripts/test-migration-gates.sh` 通过，包含 Rust workspace、Go 全包、Swift BLE 回归、carrier/USB/IMS/codec 与公开 HTTP 差分门禁；日志：[iteration-352-migration-gates.log](../build/optimization/iteration-352-migration-gates.log)。
- 契约 census 不变：343 method/path（Rust 283 / Go 60）、127 Swift 调用点、353 审阅范围；43 项契约扫描测试此前于 Iteration351 已通过，本轮未新增路由。

## 当前边界与进度

Rust 现在是恢复退避策略唯一 owner；Go 仍采集真实会话事实、运行 Go-owned VoWiFi 会话、启动下一代 session，并接收 Rust durable retry 到期通知。计入 P4 +0.025 计划人周等值，不代表 IMS/会话生命周期已经迁出 Go，也没有实机 N待长稳验收。累计 16.045 / 47.5 = 33.7789%（显示 33.8%，精算 33.78%）。整体目标未完成。
