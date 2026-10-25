# Iteration 259：租约释放Unix IPC确认与重试

日期2026-10-09，Switch_MAC。

新增真实Unix socket HTTP测试：调用实际acquire取得lease，然后release依次收到released=false、连接关闭、released=true。前两次必须失败，仅第三次明确确认才成功；核对三个释放请求都使用同一lease_id、相同release路径，共四个请求（一次acquire加三次调用），没有偷偷重新获取租约或自动重放。

定向测试通过，日志../build/optimization/iteration-259-targeted.log。服务端为fixture，证明真实Unix IPC边界，不声称真实Go读卡释放验收。首轮错误是测试将read_request字节当字符串，已修正；并行258完整门禁因此失败，最终门禁本轮重新运行：../build/optimization/iteration-259-migration-gates.log。

整体31.4%，公开生产startup关闭。未调用真实reader、外部发信或生成DMG。

最终259门禁已通过（migration gates passed）。
