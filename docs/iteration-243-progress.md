# Iteration 243：真实 TCP 归档失败不确认

日期：2026-10-09。工作区 Switch_MAC。

在共用真实内存TCP/REGISTER/actor/RegisteredConsumer fixture增加ArchiveStore失败场景：预先将sms-archive.json路径创建为目录，decoder收到有效RP-DATA后实际持久写入失败。消费循环必须报告sms_archive_unavailable，ReportSubmitted数量0，退出last_error保留此码且没有待报告ticket，归档路径仍是原目录，没有用空文件掩盖错误。

peer发送有效MESSAGE后等待失败通知，并在100ms观察窗读取实际流；任何SIP frame都会使测试失败，连接正常读EOF或仍无数据均可退役。这样验证真实wire上没有SIP成功确认或RP-ACK，不只检查模拟command队列。该路径当前没有额外错误响应策略，不将未完成USSD/unsupported方法处理混入这里。

定向日志：`../build/optimization/iteration-243-archive-tests.log`，1项通过（0.14秒）。完整日志：`../build/optimization/iteration-243-migration-gates.log`：Rust workspace 1675项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。仍为plain/SecAgree Disabled，未覆盖SIM/AKA、IKE/IPsec或公开startup；整体31.4%。无实卡/运营商、实际外部发信、应用重启或DMG。
