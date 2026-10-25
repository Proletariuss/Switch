# Iteration 272：后台停止等待真实Unix release收尾

日期2026-10-09，Switch_MAC。

Task增加request_stop以便owner请求停止并继续等待清理完成。现有真实Unix IPC fixture新增后台模式：已登记原lease和受控清理证据，实际恢复loop派发后暂停release服务；request_stop后finish保持等待，不允许重复claim；放开服务返回false，loop待该IPC结束才返回failed=1，其后退避到期可显式派发并确认成功复用slot。共用fixture仍覆盖普通与遗弃waiter路径。

定向测试通过（1.03秒），33项session模块测试通过，日志../build/optimization/iteration-272-targeted.log。公开production owner未接入，详细失败通知及跨服务重启恢复未实现；本fixture不证明真实卡片/IKE释放。整体31.4%，无实卡/发信/DMG。
