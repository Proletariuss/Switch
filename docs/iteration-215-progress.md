# Iteration 215：按读卡器/卡片代次绑定的Rust IMS事件循环

日期：2026-10-09。上一轮为实际代码和验证进展。全部修改、缓存和日志位于Switch。

## 实现

新增daemon模块`native_ims_actor.rs`，以既有`NativeSessionPermit`绑定读卡器、卡片及会话代次；消费真正的`RegisteredSipRuntime`，不是独立模拟生产实现。命令和事件通道各32项，Send/Respond命令需匹配当前generation。停止、租约丢失、消费者退出、流关闭/错误都会停止准入、关闭并拒绝排队命令，再等待IMS资源清理。

事件队列满时，actor仍处理消费者提交的应答命令，避免“消费者等待应答、actor等待消费者读队列”的互等；同样可以响应停止/租约丢失。通过预留队列容量而非直接移动event，避免非终止watch更新导致事件丢失。普通调度避免将命令置于SIP事件之前的固定优先级，以降低事件饿死风险。

`NativeTunnelOwner`将真实准备上下文的reader/SIM key传入`NativeImsNetworkControl`；新`spawn_registered_actor`校验permit所对应的key，等待actor完成IMS清理后才发IKE停止信号，并等待同一network_task收尾。permit随完成结果返回，supervisor才能释放对应generation；迟到完成不能清除替换会话。丢弃任务JoinHandle不会中断清理，未来调用方不得强制abort该owner。

增加本地`proletarius-sip`依赖用于类型化命令，复用工作区已有crate，没有引入新的外部库。

## 验收边界

生产native readiness仍关闭。该事件循环尚未由公开启动路由调用；事件消费者尚需接入完整短信持久化、RP-ACK、OPTIONS/NOTIFY、注册续期及呼叫策略，现有Go IMS owner未退出。本轮没有自动对MESSAGE发成功确认，也未宣称已完成入站短信闭环。

## 测试

新增5个actor测试：错误reader key、错误generation、排队取消、丢失租约/控制者、消费者退出、事件队列满时应答通道进展、清理完成前保留supervisor槽位、完成后替换generation及迟到结果隔离。测试driver仅用于控制事件和观察资源顺序；生产driver直接使用RegisteredSipRuntime。

日志：`../build/optimization/iteration-215-actor-tests.log`和`iteration-215-migration-gates.log`。完整Rust workspace/格式、Go全仓test/vet及载波、USB、射频、HTTP/HEAD、codec/SDP/AMR兼容性门禁均通过，不以离线检查替代生产/实机证据。

## 目标

继续实现真实事件消费者及durable SMS/RP-ACK顺序，再接入keyed启动/停止入口。全Rust加权进度维持31.4%，不把尚未被生产路由调用的actor计入正式接管完成度。本轮无实卡、真实短信、运营商连接或DMG。
