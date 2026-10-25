# Iteration 222：会话级原生短信报告分派

日期：2026-10-09。工作区 Switch_MAC。

新增 SessionConsumer，将上一轮只返回给调用方的 RP-ACK 事务纳入会话内所有权：process 首先核对 generation 和会话活动状态，随后匹配待确认报告；匹配 final response/timeout 后移除该事务并返回 ticket 与结果；其余事件继续进入已有真实持久归档 → SIP 200 → RP-ACK 路径或原样交回上层。

pending 数量限制64，达到上限时新 MESSAGE 作为 Unhandled 原样交回 session 的过载策略；不在此隐式应答成功或删除归档。控制/其他消息和 final response仍可分派。disconnected 消耗剩余事务并逐一输出 unknown；这是内存会话收尾，不是磁盘恢复，也没有自动重发。

测试把真实 ArchiveStore 写入与命令通道模拟执行接到 SessionConsumer：确认落盘之后才收到 SIP200 命令，RP-ACK 返回写入未知时不伪造 ticket、不重发；非短信保活事件保留原样、无归档/命令；其他 generation 的有效 MESSAGE 在写盘之前拒绝。既有 leaf consumer停止及故障测试保留。报告 final response 的严格关联和终态测试沿用Iteration221。

完整门禁：`../build/optimization/iteration-222-migration-gates.log`，Rust workspace 1651项测试、格式检查、Go全仓test/vet及既有协议差分全部通过，日志最终为 `migration gates passed`。新dispatcher尚未接到生产完整event-loop或公开启动路由；还需要共享CSeq/route上下文来源、报告持久恢复、USSD/voice分派及完整生命周期组合验证。不能以本轮队列与持久写入测试宣称运营商闭环。整体进度维持31.4%；无实卡变更、实际发信、应用重启或DMG。
