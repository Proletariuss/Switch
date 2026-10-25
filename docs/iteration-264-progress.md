# Iteration 264：遗弃结果后的同代次许可恢复入口

日期2026-10-09，Switch_MAC。

supervisor.recovery_permit要求key+预期generation、原entry停止信号true、存在retained资源且未claimed，才重建原stop/cancel channel绑定permit。spawn_retained_lease_recovery连接该许可到既有spawn_lease_recovery，继续核对NativeLeaseRecovery清理确认及独占claim。不会按可变当前reader猜测或为新代误重试。

实际supervisor测试：正常运行拒绝恢复、原permit drop后恢复同代资源、已claim拒绝、解除claim可重领、replacement后旧generation拒绝而新generation可取。定向通过，日志../build/optimization/iteration-264-targeted.log。前轮263完整门禁通过；本轮未重跑全仓，不以旧门禁证明新代码全仓通过。

尚无自动后台调度/公开恢复路由及跨服务重启恢复，公开startup关闭；31.4%，无实卡/外部发信/DMG。
