# Iteration 247：网络退出主动停止已注册 IMS actor

日期：2026-10-09。工作区 Switch_MAC。

已注册组合actor现在同时等待真实network JoinHandle与IMS actor。network先返回时立即请求同代次actor停止，等待其IMS清理，再保留原始network结果；actor先完成则先请求IKE停止，再等待network。没有abort、丢弃JoinError、重发短信或把网络失败替换为成功。既有registered_cleanup_complete仍要求IMS与network同时成功才能释放会话槽位。

新增两项顺序测试：network Err原样返回且actor收到停止时IKE尚未请求停止；actor已清理后网络才能收到IKE停止并返回。定向22项通过。该测试证明协调逻辑，不替代真实IKE/IPsec异常退出及卡片验收；actor自身panic恢复仍未完成。

完整门禁日志：../build/optimization/iteration-247-migration-gates.log（完整门禁通过，Rust 1678项通过，Go及协议一致性检查通过）。公开native启动仍关闭，整体31.4%。未进行实卡、外部发信、应用重启或DMG。
