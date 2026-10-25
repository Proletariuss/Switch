# Iteration 254：组合结果保留原始读卡租约

日期2026-10-09，Switch_MAC。

发现lease heartbeat.stop只停止续期，不确认release。spawn_sms_session现在在消费context前克隆同一Arc lease，所有worker结果（成功、启动失败、服务消失或JoinError）统一由NativeSmsSessionResult携带该精确租约，避免上层只能按当前reader重新寻找并释放错误租约。不增加自动release或将未确认release当成功。

cargo check通过，日志../build/optimization/iteration-254-check.log。仍有实质缺口：lease release确认必须位于supervisor槽位完成之前；当前registered worker会先标记完成，结果被放弃时也没有持久恢复owner。下一步必须把lease release纳入实际清理owner，不能凭此次保留Arc启用生产启动。整体31.4%，公开startup关闭，无实卡/发信/DMG。
