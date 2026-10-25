# Iteration 273：恢复循环保留有界类型化失败

日期2026-10-09，Switch_MAC。

NativeRecoveryLoopExit新增最近8条NativeRecoveryFailure记录，分别保留原BackendFailure、worker JoinError及observer JoinError。统计继续累计，覆盖最旧记录时omitted_failures饱和增加，不把删减误报成完整历史。记录无Debug/Serialize，不自动写日志或展示原始错误，后续UI必须遵守现有脱敏契约。

34项session测试通过，包括10次失败保持后8条、总数10、遗漏2，及真实Unix IPC后台stop/drain路径。该记录在loop结束时交付，实时按代事件/详细UI错误投影仍缺。完整门禁../build/optimization/iteration-273-migration-gates.log通过，覆盖最新assert修改。

公开startup未接入，跨服务重启恢复未完成；31.4%，无实卡/发信/DMG。
