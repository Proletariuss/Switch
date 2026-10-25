# Iteration 248：注册入口校验卡片会话与TCP存活

日期：2026-10-09，Switch_MAC。

NativeSessionContext.register_ims在读取隧道P-CSCF、生成随机事务标识、构造REGISTER或调用SIM AKA之前，核对context与network的ReaderSessionKey一致；不一致返回SessionMismatch，TCP command receiver已关闭返回TransportClosed。成功注册后既有TcpControl来源核对仍保留，两项检查分别覆盖注册前会话与注册后socket来源。

未改动运营商模板、卡片文件、原应用或外部短信行为。该入口仍处于gated状态，公开生产接管未完成。检查后网络异步退出仍可能发生，不能把一次is_closed读取当作整个注册期存活证明；注册进行中的主动网络退出协调仍需后续实现。

完整门禁日志：../build/optimization/iteration-248-migration-gates.log，已完成：migration gates passed（后续Iteration249修改后需重新验证完整门禁）。整体31.4%，无DMG。
