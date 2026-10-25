# Iteration 355：N待短信队列选择与 claim 收归 Rust

日期：2026-10-11。工作目录：`/Users/proletarius/Desktop/Switch/src`。没有连接真实读卡器或发送短信；未生成 DMG。

## 实现

N待的出站 VoWiFi 短信此前由 Go 先从 Rust 读取整批 queued 消息，再逐条生成 attempt、调用 Rust claim、交给当前 IMS session 发送并回写结果。Rust 已有耐久 claim 与 unknown 防重放，但消息选择和选择/claim 的协调仍落在 Go。

现在增加私有 `store/claim-next` 原子动作：Rust 在同一次耐久事务中校验 Profile 仍为 `keep_online`，按既有 pending 列表顺序选择首条 queued 短信，将其转为 `submitting` 并绑定 attempt token，再返回已认领消息。空队列轮询不写 journal；同一 IPC 的幂等重放只在同一 attempt 仍处于有效 submitting 状态且 desired revision 未变时返回授权；停止再启动、outcome 或进程恢复后会拒绝旧授权。请求字段严格白名单校验，并发消费者不能重复 claim 同一消息。

Go 不再读取整批 pending 列表或自行挑选 message ID，只对每个 Rust claim-next 结果调用当前代次绑定的 IMS session 发送，并将 accepted/unknown outcome 写回 Rust。发送通道、IMS session、Profile/reader 绑定和网络 I/O 仍由 Go 持有；发送结果不确定仍标记 unknown，不自动重放。Rust 请求失败时该 flush tick 结束，后续 Rust 维护节拍可再尝试仍保持 queued 的消息。

## 验证

- Rust 专项测试通过：原队列顺序、并发 claim 唯一性、空队列无事件占用、attempt 重放、过期授权拒绝、私有路由认证。
- Go `cmd/proletarius-macos` 全包测试通过。
- 完整迁移门禁：Rust workspace、Go 全仓测试与 `go vet`、Swift BLE 物理完成证明测试、载波/USB/radio/IMS/codec/HTTP parity 均通过。可复核日志：[iteration-355-migration-gates.log](../build/optimization/iteration-355-migration-gates.log)。
- 契约扫描通过：343 method/path（Rust 283 / Go 60）、127 Swift HTTP 调用点、353 fingerprinted review scopes；内部 store action 属既有动态 dispatch，不新增 method/path。`git diff --check` 通过。

## 当前进度

本轮 P4 计入 +0.025 计划人周等值，累计 **16.120 / 47.5 = 33.9368%（显示 33.9%，两位小数 33.94%）**。估值误差约 ±3 个百分点。Go 仍负责 N待 flush 生命周期、当前 IMS session 及物理网络发送；全 Rust 与实机验收均未完成。
