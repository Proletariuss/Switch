# Iteration 275：脱敏恢复最新状态订阅

日期2026-10-09，Switch_MAC。

spawn_lease_recovery_loop_with_progress返回Task及watch Receiver<NativeRecoveryProgress>。每次正常收尾及停止drain的实际result record后发布revision、confirmed/failed/task_failed/omitted计数、原generation及类型化status；无原BackendFailure、reader、ICCID、IMSI或正文。旧启动wrapper丢弃订阅仍可正常清理，不将订阅者消失视为停止条件。

watch保存最新快照而非逐条事件队列，累计计数保留；不声明完整事件历史。原始失败详情仍在有界loop exit记录内，界面/API连接尚未接管。真实Unix IPC后台stop fixture新增订阅revision=1、failed=1、原generation、ReleaseFailed断言。

34项session测试通过，日志../build/optimization/iteration-275-targeted.log。公开startup未接入，跨服务恢复未完成。整体31.4%，无实卡/外部发信/DMG。

最新完整门禁通过：../build/optimization/iteration-275-migration-gates.log。
