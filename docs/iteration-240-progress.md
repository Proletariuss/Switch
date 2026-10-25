# Iteration 240：真实内存 TCP 注册与短信 actor/consumer 闭环

日期：2026-10-09。工作区 Switch_MAC。

新增daemon组合测试：两端TunnelNetwork/真实TcpSocketRegistry在内存IP packet channels运行，客户端调用真实REGISTER coordinator（plain、SecAgree Disabled），服务端REGISTER200后立即连续发送MESSAGE，覆盖注册边界reader缓存交接。成功session捕获message_context，随后实际RegisteredSipRuntime进入native actor，RegisteredConsumer.run接同一commands/events queue。

服务端真实TCP收到SIP200时读取实际ArchiveStore，必须已经有一条短信；随后解析RP-ACK网络帧，核对PAI目标、RP reference42正文、注册认可From身份及Service-Route。服务端返回严格相关的202，消费者必须输出ReportFinished Accepted。停止后实际RegisteredImsSession.shutdown成功，consumer没有未确认报告/错误，peer观察EOF并关闭，内存network runners成功退出。

定向测试日志：`../build/optimization/iteration-240-stream-tests.log`，1项通过。仅增加已被vowifi使用的同版本smoltcp为daemon dev-dependency，Cargo.lock同步；未新增生产依赖或Go调用。初次构建修正了TunnelConfiguration所在crate与plain test的security control wiring，最终测试使用原有plain注册测试同样的未服务security runtime（plain不安装sec-agree）。

此证据比之前fixture命令通道更强：实际socket、REGISTER、MESSAGE、wire response、归档与消费循环已在同一组合运行。但没有SIM/EAP-AKA、IKE/ESP、安全REGISTER或NativeImsNetworkControl完整worker实机，也未启动公开生产路径，不能等同全VoWiFi接管。整体31.4%，无实卡/运营商、实际外部发信、应用重启或DMG。

完整门禁：`../build/optimization/iteration-240-migration-gates.log`：Rust workspace 1671项测试、格式、Go全仓test/vet和协议差分（含54组routing）通过，最终 `migration gates passed`。
