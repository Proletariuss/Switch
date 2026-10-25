# Iteration 268：跨批次全局恢复领取上限

日期2026-10-09，Switch_MAC。

claim_recovery在同一supervisor锁内统计active recovery_claimed，上限8；候选批次使用同一常量。首次release与恢复release都使用该claim，跨多批不会突破上限。失败解除claim或完成移除entry才恢复额度。原生worker实际cleanup确认后，在领取/IO之前保存清理证据；额度满返回未尝试时仍可供后续恢复，不因未poll release而丢失证据。

11项supervisor测试通过，包含10个session跨批领取最多8、解除后仅新增一个、完成后再新增一个。31项原生session模块测试通过，日志../build/optimization/iteration-268-session-tests.log。267完整门禁通过，早于本轮生产修改；不代替本轮全仓验证。

公平性/退避/后台计时调度与跨服务重启恢复仍未实现，公开startup关闭。整体31.4%，无实卡/发信/DMG。
