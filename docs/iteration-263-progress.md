# Iteration 263：首次释放与恢复共用领取仲裁

日期2026-10-09，Switch_MAC。

release_with_claim统一原生worker/已清理启动失败的初次release：owner存在时必须先claim_recovery，重复claim则不poll release。成功完成原permit，失败解除同代claim但保留slot。owner已消失则不创建替代服务，在已确认清理的上层调用边界允许release并保留结果。恢复任务先前已使用同一claim，因此两条路径不并发释放。

原释放确认前slot占用/成功/失败测试通过；新争用测试已有recovery claim时，release future若被poll即panic，实际未poll、不释放slot、未偷解除其他任务claim。两项定向通过。最新完整门禁../build/optimization/iteration-263-migration-gates.log通过。

完整恢复调度、遗弃result后的permit回收与服务重启持久恢复仍需接入。公开startup关闭，31.4%，无实卡/发信/DMG。
