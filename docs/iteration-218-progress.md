# Iteration 218：原生短信消费者的取消与unknown边界

日期：2026-10-09。上一轮完成单次事件消费者和队列命令衔接，本轮继续验证停止/代次信号的准入和结果等待路径。全部修改与测试日志在Switch内。

## 修复

消费者在SIP应答或RP-ACK命令已经入队后，等待结果期间停止、租约丢失或actor断开，不能断言远端未收到。现将这一阶段的失败分别映射为`native_sms_response_write_unknown`或`native_sms_report_write_unknown`，避免将会话停止误解为安全可重发。入队之前仍返回session_stopped/actor_closed；没有增加任何网络重试。

## 验证

新增4个消费者控制测试：满命令队列时停止撤销等待准入且无多余命令；false watch更新后仍只提交一次；租约失效终止等待写入结果并关闭回执；actor回执端或lease控制端丢失时fail closed。既有真实归档后停止测试同步验证新的unknown返回语义及没有RP-ACK入队。

完整迁移门禁日志：`../build/optimization/iteration-218-migration-gates.log`，Rust workspace/格式、Go全仓test/vet及既有全部协议兼容性门禁通过。没有访问生产数据、设备或运营商；仅有隔离测试归档及合成命令。

## 状态

原生事件消费者仍未由生产启动入口创建；完整注册续期、短信结果恢复、多段/N待、其他事件及呼叫媒体生命周期仍待迁移。原加权整体进度保持31.4%。本轮无DMG。
