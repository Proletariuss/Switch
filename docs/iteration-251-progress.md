# Iteration 251：统一原生隧道与注册启动入口

日期：2026-10-09，Switch_MAC。

新增start_registered_ims，按值消费不可Clone的NativeSessionContext，先核对permit卡片key，再启动原隧道和IMS network，调用已有注册及stop/lease监视。成功返回同源network/session所有者；注册失败先等待coordinator rollback，再请求IKE停止并await真实network任务，保留NativeImsRegistrationError及network Result/JoinError。不会发布ready或释放supervisor槽位，permit仍由上层持有。未凭单一布尔值启用公开路由。

该连接属于原生启动owner接管前的实现，尚未由公开生产接口调用。没有模拟SIM/运营商成功；真实全链及失败恢复验收未完成。settings由上层提供security endpoints、deadline及probe/retry/attempts，events保留既有调用者所有权。

前一轮250完整门禁通过。本轮cargo check通过，日志../build/optimization/iteration-251-check.log。整体31.4%，无DMG、实卡操作或外部发信。
