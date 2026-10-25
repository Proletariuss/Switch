# Iteration 277：关闭中的服务禁止新会话准入

日期2026-10-09，Switch_MAC。

NativeSessionServiceOwner新增stopping，finish_after_sessions首次请求时置true；即便active slot使shutdown返回owner，后续launch仍拒绝。实际launch共用reserve_session，先检查Stopping，再锁supervisor并reserve；失败保留原context，不触发新lease/网络/AKA。恢复loop与原session仍继续清理，下一次finish可在registry空后完成。

生命周期fixture通过实际reserve_session建立原slot，shutdown拒绝后新key准入返回Stopping且registry无新记录，原permit收到停止；同代完成后owner可关闭。35项session测试通过，日志../build/optimization/iteration-277-targeted.log。276完整门禁通过，早于本轮修改，不替代本轮全仓验证。

公开main生产owner尚未接入，真实SIM/IKE/双向业务与跨重启恢复仍需验收。整体31.4%，无实卡/发信/DMG。
