# Iteration 224：归档凭据接入共享报告帧准备

日期：2026-10-09。工作区 Switch_MAC。

Consumer 构造现在必须显式注入 NativeReportOwner（共享 Service 与验证过的 session_id）。归档返回的 NativeSmsCommit 消耗自身，先核对 generation 与原 incoming Call-ID，再调用生产事务服务的 typed wire 方法。只使用注册会话的目标、身份、local endpoint、routes、安全头与CSeq floor；报告的Call-ID、From-tag、branch及递增CSeq由共享owner生成，不使用外部手工生成的这些字段。consumer随后验证帧完整性，再沿既有原连接SIP200与tracked MESSAGE流程发送。

现有真实归档/命令通道测试验证：归档先于SIP200、RP-ACK正文与原In-Reply-To正确、实际报告CSeq75及新Call-ID；写入结果未知不重发、不伪造ticket。与consumer共用的Service克隆随后经旧入口准备报告得到CSeq76，证明原生消费路径没有绕过统一序列分配者。

NativeReportOwner不接管生产启动，不自动选取route，也不自动生成服务生命周期的session_id；这些仍需由完整原生注册owner配置，并在退出时完成retire。旧into_delivery_report保留用于既有凭据纯构建测试；原生consumer实际走新的prepare_report路径。公开生产入口仍未切换。整体31.4%，无设备操作、实际短信、应用重启或DMG。

验证日志：`../build/optimization/iteration-224-consumer-tests.log`（定向4项通过）；`../build/optimization/iteration-224-migration-gates.log`：Rust workspace 1654项测试、格式检查、Go全仓test/vet与既有协议差分全部通过，最终标记 `migration gates passed`。
