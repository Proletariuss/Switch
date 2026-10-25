# Iteration 265：保留资源恢复到真实Unix IPC闭环

日期2026-10-09，Switch_MAC。

新增实际spawn_retained_lease_recovery整体fixture：真实Unix HTTP acquire得到原lease，登记真实supervisor并停止、丢弃原permit。cleanup未标记时恢复入口返回None且不发release；受控确认清理后第一次release得到false，任务返回错误、slot继续占用且generation不变；再次显式恢复取得同一lease，服务返回true后移除旧slot允许新reserve。核对所有release path及原lease_id，服务总计一次acquire两次release。

定向测试通过，日志../build/optimization/iteration-265-targeted.log。这是真实Unix IPC客户端、实际恢复任务与supervisor闭环；服务端是fixture，清理证据受控，不声称真实IKE/卡片释放通过。完整门禁../build/optimization/iteration-265-migration-gates.log通过。

生产启动及恢复后台调度仍未公开接管，跨服务重启恢复尚缺。整体31.4%，无实卡、外部短信或DMG。
