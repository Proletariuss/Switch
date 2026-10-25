# Iteration 256：原租约释放确认控制槽位完成

日期2026-10-09，Switch_MAC。

实际组合入口复用finish_registered_lease：仅registered_cleanup_complete通过才poll release future，释放结果原样返回，成功才执行原permit worker_finished。新增受控oneshot测试在release等待期间检查同key reserve失败、任务不提前返回；释放确认后slot可复用，释放失败时completion为空且slot继续阻止重入，错误保留。测试使用实际supervisor permit与完成逻辑，release结果为受控fixture，不代替真实Go IPC/读卡器验收。

定向测试通过，日志../build/optimization/iteration-256-targeted.log；完整门禁../build/optimization/iteration-256-migration-gates.log通过（migration gates passed）。生产启动仍关闭，启动失败与ServiceGone释放/持久恢复仍待接入；31.4%，无实卡/外部发信/DMG。
