# Iteration 246：TCP 来源与运行状态分别校验

日期：2026-10-09。工作区 Switch_MAC。

TcpControl新增is_closed，直接读取真实mpsc receiver退役状态；same_registry仍只证明来源、关闭后保持identity一致。短信组合入口在key/lease/来源校验后、snapshot/report claim之前拒绝关闭runtime（native_sms_transport_closed）；独立/组合actor底层也对关闭control请求停止，仅进入已有清理路径，不继续业务IO。

新增测试用两个真实TcpSocketRegistry共用同一wake sender：clone通过same_registry，另一registry仍拒绝；丢弃一个registry后自身及clone关闭，另一个仍可用，进一步丢弃后也关闭。没有用地址相似或wake信号替代command channel来源/生命周期。

此检查不声称消除所有check后异步退出竞态；实际IO仍由既有结果/stop/lease路径处理，失败不自动重发。错误runtime的实际资源恢复与槽位重新清理仍需独立证明，公开startup关闭。

完整日志：`../build/optimization/iteration-246-migration-gates.log`（完整门禁通过：Rust 1676 项通过，Go 与协议一致性检查通过）。整体31.4%，完整AKA/IKE/IPsec及生产接管未完成。无实卡/运营商、实际外部发信、应用重启或DMG。

首次全仓回归有既有BLE APDU fixture Unix socket AlreadyExists：仅pid+SystemTime纳秒不能保证并行唯一。测试socket_path添加进程内AtomicU64序号，保留跨运行时间/pid区分；不修改生产BLE传输，随后重跑完整门禁。

第二次回归暴露既有心跳测试的并行回收竞态：过期记录仍存在返回408/heartbeat_session_expired，已被另一会话启动回收返回409/heartbeat_session_missing_or_expired。测试分别核对合法状态与错误码，仍要求旧会话被阻止及新代次递增；生产行为未修改。最终完整门禁以migration gates passed结束。
