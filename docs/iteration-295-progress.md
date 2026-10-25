# Iteration 295：转发生产者来源边界核对

2026-10-09，Switch_MAC。

核对 `device_sms_forwarding::Runtime::inbox` 与 `message_archive::forwarding_owned_list` 后确认，Rust转发producer直接读取durable archive与Rust-owned USB预览；Go的`publishInitialSMSInboxSnapshot`用于恢复兼容`GET /api/sms`的易变收件箱合并视图，不再决定转发producer是否运行。Swift `SMSForwardingStore`仅将旧游标/通知ID做一次durable handoff，Rust设备短信和N待producer均以该交接为启动条件，不存在Swift持续转发worker。

据此更正Go和Swift源码注释，避免未来把仅供列表投影的缓存publisher误删或误当作转发owner。本轮无运行逻辑变化，不计迁移比例；最后完整迁移门禁Iteration294通过。实际Go设备采集/IMS与列表缓存来源仍未迁移，31.4%，无实卡/发信/DMG。
