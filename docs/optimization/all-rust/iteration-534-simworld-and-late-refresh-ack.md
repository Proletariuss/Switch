# Iteration 534：SIM World 复测与迟到的刷新 INVITE 响应

2026-10-18。用户确认先测试读卡器中已启用的 SIM World。隔离 Rust daemon 连接 ESTKme-RED，读取到 10 个 profile，确认当前启用 SIM World。VoWiFi 隧道建立并进入 IMS `registering`，随后 `ims_transport / transport_failed / plain_connect / timed_out`；ESP 本地提交 8 包、收到 0 包、拒绝 0 包，`sms_ready=false`、`call_ready=false`。这与上一轮一致，仍不能仅凭客户端计数区分代理出口、ePDG 和本地数据面。未切卡、下载、发短信、拨号或改代理规则。已显式停止 VoWiFi、断开隔离读卡器连接并正常退出 daemon；脱敏记录在 `src/build/reports/iteration-534-simworld-sanitized.json`。实卡使用的是本轮 SIP 修改前的当前源码构建，因此不作为新通话逻辑的实网验收。

Rust IMS 通话 owner 现在保留超时或挂断时尚未收到最终响应的刷新 re-INVITE 事务。迟到的 2xx 使用对话 ACK，迟到的非 2xx 使用原 Via 分支和 CSeq 的事务 ACK；最终响应重传重复发送同一 ACK。这个路径只完成信令收尾，不重启媒体、不恢复已结束的会话定时器。旧通话退入有限历史队列后仍可 ACK 自己的迟到响应，不改变新通话状态。待决 UPDATE 仍按原清理逻辑处理，保留的 SDP 在 Call 销毁时清零。

专项 Rust 通话回归 **43 通过、0 失败**，覆盖超时后的迟到 2xx、422、重复响应和退役旧通话与新通话隔离。完整 Rust 工作区 **3068 通过、0 失败、1 忽略**；daemon 构建、`cargo fmt --check`、`git diff --check`、Go 退出检查和 294/294 Rust HTTP owner 门禁通过。测试日志在 `src/build/reports/iteration-534-*`。SIM World IMS 注册、收短信、通话及长时间在线仍未通过实机验收；下一步需要代理出口或 ePDG 的 UDP 4500/ESP 接收证据，或同卡同网已知正常客户端的严格对照。未生成 DMG。
