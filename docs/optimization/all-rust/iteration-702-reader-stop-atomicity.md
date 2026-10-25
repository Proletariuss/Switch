# Iteration 702：Reader 关闭回执与真实任务分离

同源码五组定向 138 项、0 失败，包含 701 的 14 项及本批 3 项新增测试。普通后端检查通过。717 项 Rust 输入冻结后的完整 daemon 实际测试输出为 3455 通过、1 失败、8 忽略；唯一失败位于 `archive_io::tests::real_tokio_queued_abort_waits_for_closure_drop_before_finish`。原测试观察进程中断，退出码未恢复，但测试框架明确报告失败，因此本批不授全量通过。归档测试编排竞态与后续定向修复另见 Iteration 704。

Reader 对同一原始 Record 的关闭先在私有候选 Clock 上校验并生成停止回执，只有实际 insert 和 mark_submitted 两次保存均确认成功后，才发布时钟并通知原任务停止。任何保存失败都保留原运行状态和停止信号；已提交但 ACK 未确认仍按原编号保持 unknown，不能自动重派发或借用后继任务。

新增测试使用真实 Service、保留的 intent 和共享占用：实际私有目录故障、真正保存后丢 ACK 的测试钩子，以及关闭等待超时后仍保留物理工作，恢复后实际完成 join。候选互审发现新成功用例在恢复工作前等待真实任务锁，可能自阻塞；只改测试观测为真实占用和未确认的 acquisition 回执，恢复后才等待最终 join。此测试问题的候选及审议均保留，生产修复未因此放宽。

本批不是实体 SIM/运营商或通话验收。上游代理路径、EVS 优先及 AMR-WB 回退均未修改。ADB 子进程清理风险仍在隔离候选中处理；702 冻结的 Rust 源码在该次完整回归中未漂移。

证据：`/Users/StatTrak/Desktop/Switch_MAC/build/reports/stability-702/`。P0–P6 仍未完成，整体分母未冻结，不报百分比；当前不生成 DMG。
