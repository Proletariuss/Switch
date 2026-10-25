# Iteration 229：停止与写入回执同时到达

日期：2026-10-09。工作区 Switch_MAC。

Consumer.result原先先检查active、select中也优先stop/lease，导致已可读取的actor写入回执在停止同时到达时被覆盖为session_stopped，上层进一步记为write_unknown。本轮先try_recv读取已到达证据；在select里也优先ready回执。尚无回执时停止/lease丢失仍及时取消等待，并保留unknown语义。不能将socket写入回执当作SIP最终接受。

后续命令入队仍检查active，SIP200后的第二条RP-ACK提交仍复检停止。测试安排回执先进入channel再同时标记stop，返回既有42结果；之后enqueue明确session_stopped且actor命令channel为空，避免恢复结果时继续IO。既有队列stop、false watch、lease取消、丢失controller测试保留，定向5项通过。

完整日志：`../build/optimization/iteration-229-migration-gates.log`：最终Rust workspace 1661项测试、格式检查、Go全仓test/vet及既有协议差分通过，最终 `migration gates passed`。此处仅修复真实consumer等待逻辑，没有生产公开启动或完整loop接管；仍31.4%，无实卡/运营商测试、实际短信、应用重启或DMG。

首次全仓测试暴露旧stop-after-commit断言仍要求已收到成功回执被丢弃为unknown；已改为session_stopped，继续核对无第二条RP-ACK及archive保留，随后完整重跑通过。
