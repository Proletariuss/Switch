# Iteration 242：真实 TCP RP-ACK 等待超时

日期：2026-10-09。工作区 Switch_MAC。

核对Go sendIncomingRPAck使用15秒operation context，Rust native consumer此前tracked request为20秒。本轮将原生响应等待预算对齐到15秒；Rust计时仍由注册response registry在reserve阶段开始，不声称与包含Go mutex/HTTP准备时间的整个旧operation起点完全相同。

新增第四个真实内存TCP/REGISTER/actor/consumer场景：服务端解析RP-ACK后保持TCP活跃，不发送final response；registry期限后必须输出ReportFinished OutcomeUnknown，pending被正确收尾、短信归档保留、只提交一次。peer在整个等待期间读监测，任何额外MESSAGE都会失败；客户端观察timeout后用oneshot通知peer分别退役。

初版测试25秒超时，诊断证实report timeout和actor cleanup均已发生，只是peer等待EOF未完成。DynamicEntry显式close是abort+本地移除，不保证对端收到EOF；因此修正fixture同步，不扩大保护期限、不缩短生产timeout。优雅FIN/对端关闭语义是独立待评估项，不把本地退役当作remote EOF证据。移除临时诊断打印。

定向日志：`../build/optimization/iteration-242-timeout-tests.log`，1项通过，实际耗时15.05秒。完整日志：`../build/optimization/iteration-242-migration-gates.log`：Rust workspace 1674项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。plain/SecAgree Disabled测试不覆盖SIM/AKA、IKE/IPsec或公开生产启动。整体31.4%，无实卡/运营商、实际外部发信、应用重启或DMG。
