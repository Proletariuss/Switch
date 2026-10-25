# Iteration 271：恢复后台轮询任务所有者

日期2026-10-09，Switch_MAC。

spawn_lease_recovery_loop提供内部后台Task，拒绝零poll interval；Weak supervisor、Skip missed tick，定时调用既有eligible候选/公平轮转/退避/全局claim。JoinSet持有release Task.finish，每轮本地不超过8个未收尾任务。停止/stop watch失效或服务消失后不再派发，await JoinSet中已启动release结果，不abort实际IPC。仅输出成功/失败/任务失败计数，不保存订户详情。

零interval及服务drop后自动退出测试通过，日志../build/optimization/iteration-271-targeted.log。270完整门禁通过，早于本轮改动。本轮尚未验证带实际IPC的后台停止/drain，错误详细事件交付、公开生产owner接入及跨重启恢复仍缺；计数不代替完整错误语义验收。

整体31.4%，公开startup关闭，未启动真实设备恢复、发信或生成DMG。
