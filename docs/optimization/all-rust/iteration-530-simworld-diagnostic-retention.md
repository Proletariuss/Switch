# Iteration 530：保留 IMS 启动失败的安全诊断

2026-10-18。上一轮 SIM World 隔离复测在会话清理后只显示笼统的“IMS 注册网络传输失败”，没有保留已经生成的分层诊断。本轮让 Rust VoWiFi 服务从已结束的 `SessionResult` 读取有界、脱敏的错误码、阶段和 ESP 计数，并让公共状态页在运营商配置预览后保留这份诊断；新的会话仍以自己的状态覆盖旧记录。没有新增原始 SIP/AKA、SIM 身份、密钥或 IP 记录。相关服务定向测试 **29 通过、0 失败**，完整 Rust 工作区 **3050 通过、0 失败、1 忽略**，开发版 daemon 构建通过。已审阅 `service.rs` 的实际改动，仅涉及终止结果的安全字段投影及状态消息覆盖，随后更新其整文件审阅指纹；公开接口盘点保持 294/294 Rust。

用隔离 Rust daemon 再次连接 ESTKme-RED 中的 SIM World 454/03。公开状态先显示隧道 `connected`、IMS `registering`，随后显示 `error/failed`。会话结束后仍可读取安全诊断：`ims_transport / transport_failed`，I/O 操作 `plain_connect`、种类 `timed_out`，ESP 发出 **8**、收到 **0**、拒绝 **0**。这与 [Iteration 523](iteration-523-simworld-negotiated-child-sa.md) 的独立实卡观察一致，但不能证明那 8 个 ESP 到达 Surge 出口或 ePDG。IMS 注册、收短信和通话均未通过验收。

脱敏结果在 `src/build/reports/iteration-530-simworld-sanitized.json`。实测只连接与读取读卡器、启动 VoWiFi；未切卡、下载 profile、发送短信、拨号、改 Surge 规则或生成 DMG。测试后确认 VoWiFi 停止、读卡器断开、隔离 daemon 退出；含原始卡片信息的临时运行目录已删除。
