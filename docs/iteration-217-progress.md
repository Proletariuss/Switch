# Iteration 217：原生入站短信消费者连接会话命令

日期：2026-10-09。上一轮完成归档凭据和真实writer验证，本轮继续把凭据连接到actor命令。所有源码、测试目录和日志均在Switch内。

## 实现

新增`native_sms_consumer::Consumer`，绑定准备上下文的数据根、subscription_id、session generation、actor命令发送端、停止信号和lease-loss信号。`receive`复用上一轮真实归档事务：获得NativeSmsCommit后验证RP-ACK上下文并准备请求；随后在原接收连接提交SIP 200并等待写入结果，再通过当前generation的Send命令提交RP-ACK。提交成功后返回ResponseTicket，终态仍需由事件消费者核对。

队列准入和等待应答期间都关注stop/lease loss；停止后不提交第二条MESSAGE。actor再次核对代次并在IO前检查停止。网络写入失败保留明确结果未知错误，不自动重发；归档不撤销。消费者取消后已经开始的归档或网络写入可能完成，不能据此重放网络请求。

流程复用了当前生产解码/归档writer、既有RP-ACK构造器和上一轮actor的真实命令路径。没有模拟的生产发送器，没有增加公共HTTP入口或启用native readiness。

## 验证

新增4项真实隔离ArchiveStore+类型化命令消费者测试：归档已有记录才出现SIP 200，后续RP-ACK reference/In-Reply-To正确；确认丢失报告unknown且命令队列没有重试；SIP应答之后停止使RP-ACK不入队且归档仍在；写盘失败时无任何成功网络命令；保活等未处理事件原样交还上层，不吞掉其他业务。

完整门禁日志：`../build/optimization/iteration-217-migration-gates-final.log`，完整Rust workspace/格式、Go全仓test/vet及既有载波/USB/射频/HTTP/codec兼容门禁通过。

## 剩余

该单次事件消费者尚未由生产启动路径创建；仍需完整事件循环、RP-ACK终态与恢复持久化、其他MESSAGE格式、状态报告、多段/N待策略、OPTIONS/NOTIFY、注册续期及完整呼叫媒体生命周期。Go IMS生产owner仍在，不能宣称全Rust收短信已验收。进度保持31.4%，无实卡、真实短信、运营商网络或DMG。
