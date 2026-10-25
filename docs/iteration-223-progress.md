# Iteration 223：原生 RP-ACK 帧准备与共享 CSeq

日期：2026-10-09。工作区 Switch_MAC。

将已有生产 Rust SMS transaction Service 的报告准备提取为 prepare_delivery_report_wire，直接返回 call_id、cseq 与 Zeroizing 帧字节。旧 HTTP/Go 适配入口仍复用这同一个方法，再序列化为原来的 disposition/call_id/cseq/wire_base64，因此正常输出契约保留；原生 consumer 后续可以直接调用，无需另建CSeq计数器、HTTP或base64往返。

旧 report 与发送短信两条准备路径原本只防止u32溢出，没有限制SIP CSeq的31位上界；这与已严格校验的Rust response registry不一致。本轮准备者、两类MESSAGE构建者及SmsSubmission一致拒绝 >=2^31；保留最大合法值2^31-1，耗尽后明确cseq_exhausted，不回绕、不重置。无效候选不会推进session.next_cseq。

新增验证：直接准备报告后旧适配入口继续共享序列18→19，拒绝超范围floor后下一序列仍为20；验证帧RP-ACK与Call-ID，随机标识独立。另测最大合法值后的重复耗尽，以及SMS/report构建和submission边界。既有旧适配契约测试保留。

完整门禁日志：`../build/optimization/iteration-223-migration-gates.log`：Rust workspace 1654 项测试、格式、Go全仓test/vet与既有协议差分全部通过，最终 `migration gates passed`。公开生产入口仍由原实现控制；直接方法还需接入commit receipt与session routing snapshot，而不是目前外部手动准备context。整体仍31.4%，无实卡操作、实际短信、应用重启或DMG。
