# Iteration 262：同代次独占恢复领取及显式释放重试

日期2026-10-09，Switch_MAC。

supervisor recovery_claimed阻止同代重复领取，只有同代能解除领取；claim进入Stopping并发送停止。spawn_lease_recovery先取得同代NativeLeaseRecovery及已确认清理的原lease，再独占claim，生成不abort的Task执行单次release。成功完成原permit；失败释放claim但不释放slot，原始错误返回。没有自动重试、没有创建新lease、没有公开HTTP启动。

cargo check通过；定向测试通过，日志../build/optimization/iteration-262-targeted.log。测试覆盖未登记拒绝、重复领取拒绝、解除后可再领、旧claim不能解锁新代。完整失败owner调度、遗弃结果后的permit再获取、首次release与恢复的统一仲裁、跨服务重启恢复尚未完成，因此仍不启用生产接管。

整体31.4%，无实卡/外部发信/DMG。
