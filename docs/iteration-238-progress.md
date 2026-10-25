# Iteration 238：组合任务异常与放弃等待

日期：2026-10-09。工作区 Switch_MAC。

新增3项直接运行生产join_registered_sms_workers及Task的故障测试：consumer panic作为JoinError保留，actor正常cleanup完成且consumer已退出后才登记释放；actor panic无法返回exit.permit/cleanup证据，发送stop且保持槽位占用，不伪造worker_finished；丢弃parent Task后同代次stop已发出，后台等待actor与consumer两个屏障，全部完成后自主登记并释放槽位。

测试使用真实supervisor/permit/channel和Task；actor/network成功结果为受控fixture，不是实际IMS/IKE清理验收。未把actor panic槽位保留视为恢复完成：真实panic后网络/SA释放证明和恢复管理仍是生产接管前缺口。consumer panic的报告持久恢复也尚未完成。

完整日志：`../build/optimization/iteration-238-migration-gates.log`：Rust workspace 1669项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。整体31.4%，公开startup关闭；无实卡/运营商、实际发信、应用重启或DMG。
