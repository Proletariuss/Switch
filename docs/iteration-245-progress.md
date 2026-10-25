# Iteration 245：成功注册绑定 TCP registry 来源

日期：2026-10-09。工作区 Switch_MAC。

TcpControl.same_registry使用真实mpsc command channel identity判定控制对象属于同一registry，不能靠IP/port/wake sender相似认领。RegisteredImsSession在plain/protected成功注册路径捕获services.tcp/受保护协调tcp的clone，提供belongs_to_tcp；手工session fixture没有来源证明（None），不能通过这个gate。

NativeImsNetworkControl.registered_sms_consumer在snapshot与report claim前拒绝来源不匹配（native_sms_transport_mismatch）。独立/组合actor共同底层启动也核对来源，失败对同代次发stop并走正常session清理，不进行新业务IO；原有permit key/stop/lease gate保留。这里只证明同registry，不替代lease、reader/SIM identity或AKA等独立证明。

实际protected注册测试核对local registry通过、remote registry拒绝；共用plain TCP/短信组合fixture核对local通过、独立其他registry拒绝。全部后续SIP/报告流程仍走原实现。完整日志：`../build/optimization/iteration-245-migration-gates.log`：最终源码Rust workspace 1675项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。

公开生产启动仍关闭，完整AKA/IKE/IPsec、持久report恢复及voice/USSD接续未完成；整体31.4%，无实卡/运营商、实际外部发信、应用重启或DMG。
