# Iteration 261：恢复资源携带清理确认依据

日期2026-10-09，Switch_MAC。

supervisor实际保留资源改为NativeLeaseRecovery（原lease Arc+AtomicBool cleanup_confirmed），而非裸lease。初始false；正常worker分支仅在finish_registered_lease确认实际IMS/network成功且poll release future时标记，启动失败与ServiceGone分支仅cleanup_confirmed检查通过才标记。标记发生在release尝试之前，因此release失败后原记录仍携带清理已完成证据。releasable_lease只有已标记才返回精确Arc，供后续同代次调度读取。

cargo check通过，日志../build/optimization/iteration-261-check.log；尚未实现重试请求仲裁/领取permit和持久恢复，不将标记视为完整恢复功能。260完整门禁通过（Rust1166项daemon测试通过；其workspace测试早于本轮修改）。公开startup关闭，整体31.4%，无实卡/发信/DMG。
