# Iteration 258：清理不明禁止触发租约释放

日期2026-10-09，Switch_MAC。

针对实际finish_registered_lease增加故障测试，覆盖IMS cleanup Err、真实网络JoinHandle取消产生JoinError、actor JoinHandle取消产生JoinError。release future若被poll即panic；三个分支均未poll且返回None，真实supervisor仍拒绝同卡key reserve，completion不被伪造。fixture控制IMS失败结果，JoinError由真实tokio任务产生，不代替真实IKE/读卡器故障验证。

定向测试通过，日志../build/optimization/iteration-258-targeted.log。此前256完整门禁已通过，本轮最新完整门禁../build/optimization/iteration-258-migration-gates.log失败：后续259测试将request字节误当字符串导致编译错误，已修正并在259重跑；不得计作通过。生产接管仍未启用，31.4%；无实卡、外部短信或DMG。
