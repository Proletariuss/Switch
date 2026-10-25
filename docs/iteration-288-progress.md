# Iteration 288：daemon共享原生生命周期封装

日期2026-10-09，Switch_MAC。

NativeLifecycle默认空、不启动设备，Mutex state持owner/stopping/finishing。install拒绝停止中、关闭等待中及重复owner并返回输入owner；begin关闭阻止后续安装。finish_shutdown独占取owner，在不abort的Task中实际finish_before，未确认则恢复owner，完成才清除；等待caller取消不会丢弃后台清理。恢复owner时若锁被poison，使用内层状态仅保留owner/关闭准入，仍返回unavailable，不把错误锁当正常成功。

新增共享生命周期测试验证deadline保留原owner/slot、不接收新owner，原slot完成后可再次finish。40项session模块测试重跑通过，日志../build/optimization/iteration-288-targeted.log。287完整门禁通过，早于本轮修改；caller取消及poison资源恢复还需专用验证。

尚未加入main ServerState，也未公开启用native启动。真实完整业务/跨服务持久恢复仍缺，31.4%，无实卡/外部发信/DMG。
