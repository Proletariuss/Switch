# Iteration 227：原生注册路由快照与入站身份分离

日期：2026-10-09。工作区 Switch_MAC。

native consumer与SessionConsumer接口不再接收通用DeliveryReportRequestContext，而是NativeSmsRouting：generation、注册身份、local endpoint、route/security/PANI及minimum_cseq。入站In-Reply-To、PAI目标与RP reference来自归档commit，报告Call-ID/tag/branch与递增CSeq来自共享owner。调用方不再需要/能够在原生接口提供这些报告标识。

NativeSmsCommit.prepare_report同时核对消费generation及routing.generation。其他代次的snapshot不能用于确认短信或分配报告。短信归档仍按consumer的实际订阅与事件代次执行，因此路由快照错误发生后短信保留；修正快照后按既有去重路径继续归档确认。

实测离线fixture先提交另一代次routing snapshot：返回session_mismatch、命令通道为空、报告owner序列0；随后正确snapshot在同一已归档短信上准备CSeq75，并与旧入口连续到76。既有归档错误、无效PAI、保活、stop/unknown测试保留，定向5项通过。

完整门禁：`../build/optimization/iteration-227-migration-gates.log`：Rust workspace 1658项测试、格式、Go全仓test/vet及既有协议差分通过，最终 `migration gates passed`。此snapshot仍需由真实注册结果/transport/security lease构建；本轮只完成接口分离与代次校验，没有生产公开启动或完整loop接管。整体31.4%，无实卡/运营商测试、实际发信、应用重启或DMG。
