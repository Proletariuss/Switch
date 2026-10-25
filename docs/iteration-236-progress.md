# Iteration 236：注册 actor 与短信循环组合 worker

日期：2026-10-09。工作区 Switch_MAC。

NativeImsNetworkControl.spawn_registered_sms在同一permit、注册session及network control下创建有界commands/events、固定registered consumer和通知循环，再启动实际RegisteredSipRuntime actor。返回同一代次取消的detached-safe Task；消费方准备失败也把stop置true并通过正常actor的RegisteredImsSession.shutdown → IKE停止 → network task收尾，不能丢弃已存在的连接或隧道。

组合模式将原spawn_registered_actor完成登记延后：底层actor仍mark_running并完成IMS/network cleanup，但不提前worker_finished。父worker等待actor，随后请求同代次stop、等待consumer task（包含正在执行的archive/notification收尾），最后以exit.permit登记完成，避免consumer未结束时释放槽位。独立actor仍保留原网络收尾后登记模式。结果保留actor/network/completion、consumer LoopExit/JoinError或setup失败码。

这次组合路径实际引用所有真实实现，而非另建假socket/档案服务，但公开startup尚未调用。新增组合代码的编译及既有actor/队列/归档/注册测试不能当作完整端到端组合验收；组合失败、panic及取消场景仍需新增专门验证，report持久恢复、USSD/call下游也未完成。

完整日志：`../build/optimization/iteration-236-migration-gates.log`：最终源码Rust workspace 1664项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。整体31.4%，无实卡/运营商、实际发信、应用重启或DMG。
