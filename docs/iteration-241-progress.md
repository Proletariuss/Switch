# Iteration 241：真实 TCP 报告拒绝与断线

日期：2026-10-09。工作区 Switch_MAC。

将上一轮实际内存TCP/REGISTER/actor/RegisteredConsumer组合测试共用同一exchange fixture，保留202 Accepted，新增403及report提交后peer close两项。不是重复假command response：服务端实际解析RP-ACK帧并返回SIP final或关闭真实ManagedTcpStream。

403必须由严格相关的response驱动ReportFinished Rejected(403)，没有report重发；peer close用oneshot等待consumer已报告ReportSubmitted后再关闭，避免混淆socket写入不确定和确定写入后没有final response，退出必须保留1个OutcomeUnknown报告。所有场景SIP200前已有真实ArchiveStore记录，报告结果不能删除该短信，报告仅提交一次，双方socket/network正常退出。

定向日志：`../build/optimization/iteration-241-stream-tests.log`，3项通过。共用fixture减少重复网络setup，仍为plain register、SecAgree Disabled；未覆盖SIM/AKA、IKE/IPsec、安全REGISTER或实际运营商/公开启动。

完整日志：`../build/optimization/iteration-241-migration-gates.log`：Rust workspace 1673项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。整体31.4%，实际全Rust生产接管未完成。无实卡变更、外部发信、应用重启或DMG。
