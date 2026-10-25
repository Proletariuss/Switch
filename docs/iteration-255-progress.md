# Iteration 255：组合入口释放原租约后完成槽位

日期2026-10-09，Switch_MAC。

spawn_registered_sms_with_completion允许上层延后finalize；旧低层入口仍维持已有行为供fixture使用。spawn_sms_session实际组合入口使用false，不让worker先完成槽位。await worker actor+network证明registered_cleanup_complete后，await同一Arc lease.release；仅确认成功才调用同一permit的worker_finished。NativeSmsSessionResult新增lease_release Option<Result>保留未尝试/确认/失败区别。任何未知IMS/network结果不发起释放，释放失败保留slot。consumer已await后才进入本阶段。

cargo check通过；现有原生session26项定向测试通过（不代替专用release顺序验收），日志../build/optimization/iteration-255-check.log及iteration-255-targeted.log。仍需专用释放顺序/失败测试与真实IPC验证。启动失败/ServiceGone分支未自动释放，外部遗弃失败结果仍需恢复owner。公开startup关闭，31.4%，无实卡、外部发信、DMG。
