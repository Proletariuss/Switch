# Iteration 342：按发送方删除的实体短信事务迁入 Rust

2026-10-10。变更和测试材料仅位于 `/Users/proletarius/Desktop/Switch`。本轮使用假 USB AT 传输，没有操作实体 SIM、发送短信、拨号或生成 DMG。

## 变更

- 公开“按发送方删除”仍由 Rust 冻结归档和 USB 预览快照；当存在实体来源时，Rust 直接以发送方匹配当前完整证明集，并将每条消息的卡身份、所有存储分区和 PDU 来源交给 Rust USB AT owner 执行。
- 删除操作不再调用 Go `/api/internal/sms/delete-sender/physical` 适配器，也不依赖 Go 申请的实体存储 lease。证明不完整、会话过期或来源变化仍失败关闭；消费证明后遇到不确定结果不自动重试。
- 设备结果部分成功时，继续只同步已确认删除的 ID；本地归档与 Go 重复缓存同步失败仍返回错误并要求刷新核对。完整成功才按冻结的发送方快照清理本地记录。
- 没有增加 HTTP 路由；契约扫描结果仍是 343 个 method/path（Rust 282 / Go 61）、127 个 Swift 调用点和 354 个审阅范围。契约审阅记录已更新。

## 验证

- 新增 owner 集成测试，证明请求只展开为当前发送方对应的精确删除组，包含卡身份与全部来源 PDU，并经无 Go lease 的 Rust owner 路径确认后才清理预览。
- 定向服务/策略测试通过。
- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,872 passed / 6 ignored**，daemon **1,337 passed / 3 ignored**；Go 全包/vet、Swift BLE、carrier replay、USB/radio/HTTP/codec/SDP/MESSAGE 差分通过。
- 迁移契约扫描与 `git diff --check` 通过。
- 日志：[Iteration 342 migration gates](../build/optimization/iteration-342-migration-gates.log)。

## 进度与边界

P2B 增加 **+0.025 计划人周等值**，累计 **6.300 / 8.0 = 78.8%**；全项目累计 **15.820 / 47.5 = 33.3%**（精确比值 33.31%，估值误差约 ±3 个百分点）。与上一轮的 33.3% 显示值相同是因为仪表按一位小数取整，分子已从 15.795 增至 15.820。

公开单条删除与按发送方删除已由 Rust 直接执行。Go 仍保留实体短信清空、短信发送硬件适配、重复缓存修剪和 standalone 兼容入口；实体设备尚未验收，模块 reader 生命周期、VoWiFi/IMS 在线会话、N待网络、语音及零 Go 构建/运行依赖仍未完成。9eSIM `6982` 不在范围内。
