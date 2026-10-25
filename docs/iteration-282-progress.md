# Iteration 282：显式释放完成后的有界确认记录

日期2026-10-09，Switch_MAC。

为仍在迁移中的Go读卡租约桥保留独立确认记录：显式release等待pcsc回调、watchDone及finishNativeReaderSessionLease完成后才记录原tokenHash；原始token不存储。最近16条，5分钟内有效，恒定时间hash比较；过期、未来时间、错误token及淘汰记录均不提供证明。expiry/cancel路径不记显式成功。已有release API重复请求仍失败，不改既有错误语义。

专用纯记录测试及已有实际私有release handler+PC/SC fixture测试通过，后者核对成功释放确实记proof且重复release仍拒绝。日志../build/optimization/iteration-282-targeted.log。281完整门禁通过。仅记录部分，尚未增加只读查询接口或Rust客户端恢复，不声称迟到确认丢失已解决。

这是兼容桥阶段的临时私有协议支撑，未减少Go依赖；最终Rust物理owner接管后须迁移该确认语义并删除Go实现。31.4%，无实卡、外部发信或DMG。
