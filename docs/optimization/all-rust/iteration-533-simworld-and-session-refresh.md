# Iteration 533：SIM World 实卡复测与无 UPDATE 对端的会话刷新

2026-10-18。使用当前源码在隔离数据目录运行 Rust `switchd-rs`，连接 ESTKme-RED，确认已启用的号卡为 SIM World。VoWiFi 建立 IKE/EAP 隧道后进入 IMS `registering`，最终在 `plain_connect / timed_out` 失败；ESP 本地提交 **8** 包、收到 **0** 包、拒绝 **0** 包，`sms_ready=false`、`call_ready=false`。Surge 本次记录选择“香港 13 家宽”，对应会话出站 2051 字节、入站 1188 字节、`failed=false`。入站总量包含 IKE 回复，不能据此认定 ESP 有回包；13、14、15 家宽节点出现同类结果，也不能仅凭客户端计数定位到代理出口或 ePDG。脱敏结果在 `src/build/reports/iteration-533-simworld-sanitized.json`。本次未切卡、下载、发送短信、拨号或改代理规则；测试后明确停止 VoWiFi、断开读卡器、退出隔离 daemon，并删除隔离运行目录。此实卡测试发生在本轮 SIP 刷新代码修改之前，因此不构成新刷新逻辑的实网验收。

Rust IMS 通话 owner 对会话定时器刷新增加能力判断：对端 `Allow` 明确包含 UPDATE 时使用无 SDP UPDATE；未声明支持时，携带字节不变的当前本地 SDP 发起 re-INVITE；UPDATE 返回 405/501 时也切换为 re-INVITE。re-INVITE 最终响应由刷新事务处理，2xx 用对话 ACK、非 2xx 用原分支和 CSeq 的事务 ACK；200 SDP 答案须保持已选编码、payload 与可用媒体方向，失败时 ACK 后 BYE。422/Min-SE 有界重试，最终响应重传重复 ACK；本端刷新未决时以 491 拒绝冲突的远端 UPDATE/re-INVITE，避免丢失原事务。入站媒体更新后保留最新本地 SDP，刷新不改变 SDP origin/version。此项是离线 SIP 兼容性改动，不会解决 SIM World 当前的 ESP 回程缺口。

专项测试覆盖无 UPDATE、405 回退、422/Min-SE、无效 SDP、ACK 重传及并发冲突。最终完整 Rust 工作区 **3065 通过、0 失败、1 忽略**；daemon 构建、`cargo fmt --check`、`git diff --check`、Go 退出检查和 294/294 Rust HTTP owner 门禁通过，日志在 `src/build/reports/iteration-533-*`。SIM World IMS 注册、收短信、通话和长时间保持仍未验收。下一步需要代理出口或 ePDG 侧的 UDP 4500/ESP 接收证据，或同卡同网已知正常客户端的严格对照，才能区分网络丢包与实卡数据面不兼容；不根据现有计数盲改模板。未生成 DMG。
