# Iteration 226：入站 P-Asserted-Identity 报告路由

日期：2026-10-09。工作区 Switch_MAC。

原Go sipMessageDeliveryReportRoute以P-Asserted-Identity选择IP-SM-GW，不以From为替代。Rust新增incoming_delivery_report_target，解析显示名/尖括号、裸地址、地址列表、sip/sips；重复header或列表按旧实现选择第一个身份，避免拒绝运营商的多身份格式。缺失、不支持的scheme、未闭合尖括号、wildcard及控制字符不形成目标。

NativeSmsCommit保存由真实入站headers提取的目标（Zeroizing）。实际consumer报告准备使用该目标，外部context.target不能覆盖。无法形成目标时仍先完成短信持久归档，随后返回invalid_delivery_report_route，不分配新CSeq或提交成功确认/RP-ACK。这不是完整未支持SMS/USSD响应策略；生产native门禁保持关闭。

新增协议格式测试；实际consumer测试故意传错误手工目标，确认最终MESSAGE仍使用PAI里的smsc。无效tel asserted identity测试验证真实ArchiveStore已有一条短信，命令通道无SIP成功应答或报告，CSeq仍为0。既有归档/unknown/共享序列测试保留。

完整日志：`../build/optimization/iteration-226-migration-gates.log`：最终源码重跑完整门禁通过，Rust workspace 1658项测试、格式、Go全仓test/vet及协议差分通过，日志为 `migration gates passed`；本轮初次编译发现新增测试缺少类型导入，已修正并重跑。最终多身份兼容调整后已重新完成全套回归。整体31.4%，生产完整loop、注册route上下文、持久报告恢复仍待完成。无实卡/运营商测试、应用重启或DMG。
