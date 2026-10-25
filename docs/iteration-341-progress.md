# Iteration 341：公开短信删除由 Rust owner 执行实体存储事务

2026-10-10。变更和测试材料仅位于 `/Users/proletarius/Desktop/Switch`。本轮使用假 USB AT 传输，没有操作实体 SIM、发送短信、拨号或生成 DMG。

## 变更

- 公开单条实体短信删除现在由 Rust 路由校验当前 Rust 收件快照中的删除令牌，再直接进入 USB AT owner；不再经过 Go 的物理删除适配器。
- USB AT owner 在一个独占事务中检查 reader/session、现有卡片租约、存储忙碌与射频抑制状态，执行卡身份和 PDU 复核、CMGD 删除及 CPMS 恢复。状态不确定时不自动重试。
- 设备确认后，Rust 才删除耐久归档记录；Go 仅按确认的稳定 ID 修剪重复易变缓存。Go standalone 兼容入口仍保留。
- 未新增路由；当前清单仍为 343 个 method/path（Rust 282 / Go 61）、127 个 Swift 调用点和 354 个审阅范围。

## 验证

- 假设备覆盖既有 SIM 租约下拒绝删除且不发送卡命令，以及完整身份复核、物理删除和存储选择恢复。
- 服务集成测试证明：当前 Rust 快照签发的删除令牌会转换为精确卡片身份、存储位置和 PDU 来源证明，经 Rust owner 执行；仅接受设备结果后修剪 Go 预览缓存。
- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,871 passed / 6 ignored**，daemon **1,336 passed / 3 ignored**；Go 全包/vet、Swift BLE、carrier replay、USB/radio/HTTP/codec/SDP/MESSAGE 差分通过。
- 日志：[Iteration 341 migration gates](../build/optimization/iteration-341-migration-gates.log)。

## 进度与边界

P2B 保守增加 **+0.025 计划人周等值**，累计 **15.795 / 47.5 = 33.3%**（估值误差约 ±3 个百分点）；P2B 为 **6.275 / 8.0 = 78.4%**。本轮只计一项生产路径从 Go 移交 Rust USB AT owner 的职责。实体设备尚未验收；Go 仍保留短信清空、按发送方删除、发送硬件适配与 standalone fallback，模块 reader 生命周期、VoWiFi/IMS 在线会话、N待网络、语音及零 Go 构建/运行依赖均未完成。9eSIM `6982` 不在范围内。
