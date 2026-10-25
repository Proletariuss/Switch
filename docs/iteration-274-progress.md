# Iteration 274：恢复失败记录关联原会话代次

日期2026-10-09，Switch_MAC。

dispatch_lease_recovery返回NativeRecoveryTask携带原generation和原Task，后台JoinSet结果保留generation。NativeRecoveryFailureRecord保存Option<SessionGeneration>+原类型化失败；正常release/worker失败归属该代，outer observer JoinError无确定归属时None，不虚构任意号卡。仍无reader/ICCID/IMSI序列化或Debug输出。

真实Unix IPC后台停止fixture增加失败record.generation==原generation断言。34项session模块测试通过，日志../build/optimization/iteration-274-targeted.log。273完整门禁通过，早于本轮修改。实时错误事件/界面脱敏投影和公开production owner仍需接入，跨服务重启恢复未完成。

整体31.4%，无实卡/外部短信/DMG。
