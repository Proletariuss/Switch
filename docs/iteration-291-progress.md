# Iteration 291：退出失败不等于释放成功

2026-10-09，Switch_MAC。

修正Iteration290的错误分支：原生清理无论返回超时、任务失败或生命周期不可用，都不应提前终止Go租约桥。等待器保持清理监听器可服务，错误类型变化时输出静态诊断，重复同类错误不刷屏；直到清理确认才允许继续正常退出。永久失败可能保持退出等待，不能据此开启生产原生会话。

NativeLifecycle增加清理失败锁存：恢复任务JoinError或外层关闭任务JoinError不能因owner已取走而在下次finish时变成空容器成功。保留stopping状态和失败原因。加入实际恢复worker受控panic测试、两类失败重复检查测试，以及实际Unix回调连接跨失败重试后完成的测试。

28项cleanup定向测试通过（运行时尚未加入最后的worker panic测试）；最终新增测试由最新完整门禁覆盖。日志 ../build/optimization/iteration-291-targeted.log、iteration-291-migration-gates.log。完整门禁已通过，退出码0；最终新增任务panic测试也已通过。

尚未启用生产原生会话；永久锁异常的恢复/运维退出策略与实机关闭验收待完成。31.4%不变，无实卡/发信/DMG。
