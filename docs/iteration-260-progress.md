# Iteration 260：同代次恢复资源由supervisor持有

日期2026-10-09，Switch_MAC。

SessionEntry新增opaque Arc<Any+Send+Sync> recovery，不引入网络或读卡操作依赖。retain_recovery只允许存在的同代次首次登记，拒绝覆盖；typed recovery只返回同代次资源。spawn_sms_session在隧道启动前登记精确lease Arc，失败或owner不存在则请求stop。worker_finished移除该entry时才释放资源，调用者放弃结果不会直接丢失服务内原lease。

实际supervisor测试证明外部Arc drop后仍保留、确认完成后释放、旧permit无法向新代登记/读取/清除资源。cargo check与定向测试通过。仅持有恢复资源，尚未实现失败slot的完整重试调度、重建permit或服务退出后的持久恢复；不能声明这些功能已完成。

259完整门禁通过；本轮完整门禁../build/optimization/iteration-260-migration-gates.log已通过。公开startup关闭，31.4%，无实卡/发信/DMG。
