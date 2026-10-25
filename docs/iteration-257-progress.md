# Iteration 257：已确认清理的启动失败释放原租约

日期2026-10-09，Switch_MAC。

NativeRegisteredStartupFailure.cleanup_confirmed核对原始结果：ServiceGone要求IMS JoinHandle+cleanup成功及network成功；Registration要求network成功且真实RegistrationFailure.cleanup.is_clean，Endpoint/Entropy为尚未创建注册资源的失败。会话不匹配、TCP已关闭、Tunnel失败一律不作释放证明。

spawn_sms_session失败分支在上述证明后等待同一lease.release；确认成功才worker_finished原permit。ServiceGone则在服务已经消失后确认释放并保留结果，不伪造supervisor。未知结果或release失败保留permit/租约/错误，没有重试或吞掉失败。

cargo check通过，日志../build/optimization/iteration-257-check.log。专用失败分支与IPC验收仍缺；前轮256完整门禁运行中，其Rust workspace测试早于本轮修改。公开startup关闭，31.4%，无实卡/外部发信/DMG。
