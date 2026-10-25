# Iteration 253：启动前同步拦截已停止会话

日期：2026-10-09，Switch_MAC。

start_registered_ims在key校验后同步读取permit停止状态，已停止或失效watch返回Tunnel(Stopped)。start_tunnel自身也在建立lease heartbeat、创建IKE停止转发器及打开transport之前检查相同状态，避免仅依赖异步forwarder观察预先停止。后续启动过程仍使用既有停止/租约失效监听；不声称消除检查后所有异步竞态。

cargo check通过，原生session模块26项定向测试通过，日志../build/optimization/iteration-253-check.log与iteration-253-targeted.log。前轮252完整门禁已通过；其全仓Rust测试早于本轮修改，不能替代本轮全仓验证；公开startup关闭，31.4%。无实卡、外部发信或DMG。
