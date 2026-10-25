# Iteration 235：常驻原生短信消费循环

日期：2026-10-09。工作区 Switch_MAC。

RegisteredConsumer.run现在直接持有actor Event receiver与有界LoopNotice sender，串行调用实际registered consumer处理event。输出SessionOutcome（报告提交/完成、原始Unhandled非短信事件）或静态错误码。无独立网络重试、无并发归档/确认task。时间来自系统UNIX epoch毫秒，非法时间明确失败，不伪造0。

循环监听stop、lease和observer关闭；通知队列满时reserve等待仍被stop/lease唤醒，false watch update不移动或丢弃待通知对象。退出返回reason、last_error、剩余报告的unknown状态，以及尚未送达observer的原始通知（含已知final result或Unhandled event），避免因停止而静默丢弃已获得的结果。consumer/input释放后actor自身通过channel关闭处理网络退出，完整task join顺序仍需组合验收。

新增2项通知队列测试：满队列false update后通知只送一次，stop时不额外入队且原通知仍保留；observer关闭与lease失效区分。既有真实ArchiveStore与registered协议状态测试保留。上述测试验证共享的实际queue helper，不等于整个run已与真实注册/actor完成端到端验证。

完整日志：`../build/optimization/iteration-235-migration-gates.log`：最终源码Rust workspace 1664项测试、格式、Go全仓test/vet和既有协议差分（含54组routing）通过，最终 `migration gates passed`。run尚未由公开startup调用，voice/USSD的Unhandled接续及完整生命周期组合仍待完成。整体31.4%，无实卡/运营商、实际发信、应用重启或DMG。
