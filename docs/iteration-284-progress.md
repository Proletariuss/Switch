# Iteration 284：原生释放失败按原token只读确认

日期2026-10-09，Switch_MAC。

release_with_confirmation调用原release；成功不查询，失败只查原lease_id release-status，明确true才成功，false或查询错误保留原BackendFailure。原release方法及普通计划清理行为不改。原生启动/worker/恢复四处实际释放改用该方法，仍须先证明IMS/network清理，再持独占claim；不存在凭租约404/409或读卡器当前状态推断成功。

新增真实Unix IPC四场景通过：release false+proof true、false+proof false、false+旧桥404、断连+proof true。核对同一原token，两次请求仅release和只读query，不立即重放。proof false/404核对原释放错误精确保留。37项session测试通过，共用恢复fixture增加status false请求，保持原slot/退避/再恢复语义。

日志../build/optimization/iteration-284-confirmation-tests.log及iteration-284-session-tests.log。283完整门禁通过，其workspace测试起于本轮修改之前；本轮最新门禁iteration-284-migration-gates.log通过。仍需完整真实Go回调迟到确认及main关闭接入验收。31.4%，无实卡/外部短信/DMG。
