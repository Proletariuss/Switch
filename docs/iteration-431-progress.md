# Iteration 431：IMS OPTIONS 确认重试 deadline 移入 Rust

VoWiFi OPTIONS 主探测未通过后，Rust heartbeat session 负责等待既有的 3 秒确认期限。Go 不再建立本地 `time.Timer`，而是通过现有认证 IMS codec UDS 接口等待。Rust 读取 session 中 `retry_not_before`，等待时释放共享锁，醒来后重新核验 session、generation、确认阶段、deadline 与过期状态；取消请求不会推进阶段或消费确认机会。Go 仍保留主探测失败后的 readiness 降级、同一 CSeq 的确认 SIP 请求、恢复回调以及最终故障 teardown。

本轮仅移动 retry deadline 的 owner，没有改变确认间隔、OPTIONS 报文、响应门限或失败后关闭 VoWiFi 的策略。当前 primary cadence 和 confirmation retry 两类心跳 timer 均已在 Rust；Go 继续做受保护 SIP socket 发送/响应等待与 transport 生命周期。

按计划职责权重 P3 +0.025，全项目进度 **17.920 / 47.5 = 37.73%**（误差约 ±3 个百分点）。Go 退出审计仍有 6 类阻塞及 264 个生产 Go 文件；全 Rust 目标尚未完成。

定向确认 deadline 测试通过，Rust `proletariusd` 全套 1,432 passed / 3 ignored；Go IMS 与 daemon bridge 测试及 race 通过；迁移清单差异与 44 项扫描器工具测试通过。全量 migration gate 和可复核日志待记录；无实体卡/运营商实测，本轮不生成 DMG。
