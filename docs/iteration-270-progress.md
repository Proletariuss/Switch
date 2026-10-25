# Iteration 270：原租约释放失败指数退避

日期2026-10-09，Switch_MAC。

同代entry记录recovery_failures及单调Instant deadline；实际首次release和恢复release失败调用recovery_failed，解除claim但保留slot及原资源。延迟1/2/4/8/16/32/60秒、最高60秒，失败计数饱和不回绕。候选、许可恢复及直接claim均拒绝deadline之前的请求；无定时器或自动发起IO。

专用测试通过：失败后所有入口拒绝、slot仍占用、各级退避上限准确。31项session测试通过，真实Unix IPCfixture第一失败后观察deadline到期再显式派发，正常/遗弃waiter均通过（约1.02秒）。日志../build/optimization/iteration-270-targeted.log与iteration-270-session-tests.log。269完整门禁通过，本轮完整门禁iteration-270-migration-gates.log通过。

自动计时后台调度、跨服务持久恢复及真实设备验收尚缺。公开startup关闭，整体31.4%，无实卡/外部发信/DMG。
