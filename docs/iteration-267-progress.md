# Iteration 267：有界原租约恢复派发

日期2026-10-09，Switch_MAC。

recovery_candidates在实际supervisor中按资源类型/资格过滤后再截取，每批最多8条，只包含已停止且未claimed的同代记录。dispatch_lease_recovery进一步要求NativeLeaseRecovery清理已确认，再通过原key+generation许可恢复和独占claim启动任务。只执行显式单次pass，无定时器或自动重试。

候选测试12条验证零limit、每批8条、先过滤后截断、已claim排除及错误资源类型排除。既有真实Unix IPC恢复fixture改为派发入口启动，正常失败/再恢复和放弃waiter均通过。候选定向+31项session测试通过；完整门禁../build/optimization/iteration-267-migration-gates.log通过。

8条是单批上限，不是跨多次pass的全局并发上限；后台公平性/退避/全局并发及服务重启持久恢复仍未实现。公开startup关闭，31.4%，无实卡/发信/DMG。
