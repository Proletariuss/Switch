# Iteration 239：完成登记要求清理成功证据

日期：2026-10-09。工作区 Switch_MAC。

检查发现组合和独立actor在Join返回Ok tuple时可能不论内部cleanup/network结果都登记worker_finished。本轮统一registered_cleanup_complete：必须Exit.cleanup为Ok且network task为Ok(Ok(()))，才允许登记并释放槽位。IMS cleanup报告错误、network JoinError、network运行错误均保留原始结果和completion=None，不把任务结束当作资源释放成功。

新增同一生产join helper的故障测试，覆盖上述3类错误，真实supervisor保持slot占用，清理证据判定为false。既有正常退出、setup失败等待、consumer panic与actor panic测试保留。fixture错误只验证控制策略，不证明真机异常恢复。

未把槽位保留作为恢复完成：需要后续以实际资源证明完成重清理或明确故障恢复，才能开放新的同reader session；当前没有自动强制释放、盲目重启或重发。公开startup仍关闭，整体31.4%。无实卡/运营商、实际发信、应用重启或DMG。

完整日志：`../build/optimization/iteration-239-migration-gates.log`：Rust workspace 1670项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。
