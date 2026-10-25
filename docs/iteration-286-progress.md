# Iteration 286：有期限的关闭等待保留原owner

日期2026-10-09，Switch_MAC。

NativeSessionServiceOwner.finish_before提供等待slot清理期限：先begin_shutdown、拒绝新准入并保持恢复loop，定期检查registry；超时或锁不可用返回原因+原owner，保留slot、原租约恢复资源及任务；registry空后复用实际finish/drain。等待期间调用方可并行泵认证cleanup callbacks，该函数不替代main的callback listener。

新测试验证deadline返回原owner、stop已传播、恢复task仍持有、slot不删、新准入拒绝，原slot确认后仍能最终关闭。实际Unix IPC服务关闭fixture改用该等待接口，覆盖false失败/查询false/退避/后台确认成功后完成关闭。

首轮编译锁临时借用阻止移动self，已改为先生成empty快照释放锁再返回owner。39项session模块测试重跑通过，日志../build/optimization/iteration-286-targeted.log。期限针对slot等待，最终recovery drain仍需真实完成，不宣称能强制安全中断IPC。

main回调泵与公开owner仍未接管，31.4%，无实卡/外部发信/DMG。

最新完整门禁通过：../build/optimization/iteration-286-migration-gates.log。
