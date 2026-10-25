# Iteration 250：注册停止必须等待回滚结果

日期：2026-10-09，Switch_MAC。

将register_ims内的停止/注册竞态协调提取为实际入口调用的finish_registration_after_stop。停止触发只设置coordinator stop，再await同一注册future；正常注册完成不额外请求停止。

通过oneshot控制回滚完成时间，验证收到stop但回滚未完成时owner保持等待，释放后返回原始rollback_failed；正常完成7保留且stop=false。相关定向8项通过。此项证明协调future与结果保留，不声称真实SIM/IKE资源回滚全部完成。外部丢弃整个注册owner的恢复仍为缺口，公开生产startup未接管。

完整门禁日志：../build/optimization/iteration-250-migration-gates.log，完整门禁通过（migration gates passed）。整体31.4%，无实卡/外部短信/DMG。
