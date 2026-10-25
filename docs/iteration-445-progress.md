# Iteration 445：poisoned supervisor shutdown still drains owned work

日期：2026-10-13

## 改动

- `NativeSessionServiceOwner::begin_shutdown` 在 supervisor 锁正常时先对 session generation 发出 stop，再请求 owner-held session task 协作退出；仍保留 recovery worker，直到 session slots 与任务确实清空，避免中途停止一个正在执行或尚待启动的 lease recovery。
- supervisor mutex 中毒时无法确认 slot/recovery 状态，shutdown 仍向 owner-held session tasks 和 recovery worker 发出 stop，然后返回 `SupervisorUnavailable`。上层保持清理失败状态，不把未确认释放报告成成功。
- 新增故障注入测试：毒化 supervisor mutex，确认已持有的 session task 和 recovery worker 均完成退出、session task panic 被保留；既有“后台 lease retry 必须先确认原 lease 才能关闭”回归继续通过。

## 验证与范围

- 定向测试 `service_shutdown_waits_for_background_retry_to_confirm_original_lease`、`shutdown_requests_owned_task_cleanup_before_reporting_poisoned_supervisor` 均通过。
- daemon 全量测试：**1453 passed / 3 ignored**。
- 本轮修正了第一次全量验证揭示的 shutdown 顺序回归；保留“session recovery 未完成时不能停 recovery worker”的原语义，最终全量套件通过。
- 启动输入仍未接入生产 owner；VoWiFi、IMS、SMS/call gates 不变，迁移进度仍 **37.99%**。未操作实体卡/运营商网络，未生成 DMG。

## 后续

继续打通真实 session startup 所需的单一权威输入与 owner 生命周期，不开放没有完整 IMS/IPsec/SMS/call cleanup proof 的生产入口。
