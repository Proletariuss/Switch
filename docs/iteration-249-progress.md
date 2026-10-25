# Iteration 249：注册期间监听网络关闭、停止与租约丢失

日期：2026-10-09，Switch_MAC。

TcpControl.closed直接等待真实command receiver关闭，包含开始等待之前已关闭的状态。register_ims为注册协调器提供独立stop signal，初始状态继承用户停止、失效watch、租约丢失和已关闭TCP；注册期间同时监听上述来源。触发后只发送停止，继续await原注册future执行已有rollback，保留其真实成功/失败结果，不abort注册。若最后完成与停止并发而仍得到session，该session仍由既有actor gate和清理owner处理，不能自动视为公开启动成功。

两项定向测试通过：真实registry退役唤醒等待；false更新不终止等待，租约true才终止。未证明完整真实SIM AKA或IKE/IPsec注册rollback，也未解决外部取消整个register_ims future的所有者恢复；公开启动仍关闭。

前一轮248完整门禁已通过。本轮最新完整门禁日志：../build/optimization/iteration-249-migration-gates.log，已通过（migration gates passed）。整体31.4%，未操作实卡、发信、重启应用或生成DMG。
