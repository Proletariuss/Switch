# Iteration 529：Rust IMS 通话会话定时器

后续更正：本轮对不含 `Session-Expires` 的成功刷新响应执行挂断、对不带该头的入站 UPDATE 保留旧期限，这两项不符合 RFC 4028；已在 [Iteration 531](iteration-531-rfc4028-timer-correction.md) 修正。以下记录保留本轮实际实现和测试结果。

2026-10-18。在 `native_ims_calls` 中补齐已建立通话的 SIP 会话刷新：当 2xx 协商本端为刷新方时，在间隔一半处发送不带媒体体的 UPDATE，并只接受同连接、同 Call-ID、同 CSeq/事务标识的响应。成功响应重置期限；422 的 Min-SE 可触发一次提高间隔重试；408、481、超时或到期发送 BYE 并清理媒体。由远端负责刷新时，本端不主动发 UPDATE；经同一对话收到的带 Session-Expires 的 UPDATE 或 re-INVITE 可更新期限，普通不带定时器头的 UPDATE 不清除已有期限。入站 INVITE 的 200 应答携带定时器头，最短间隔随协商保持。

专项测试覆盖主动刷新及 2xx 续期、422 提高 Min-SE、408 结束、远端未刷新到期、入站协商与 UPDATE、错误连接隔离、缺失定时器响应的清理。最终专项 **25 通过、0 失败**；完整 Rust 工作区 **3050 通过、0 失败、1 忽略**。Rust-only 接口盘点仍为 294/294，三处因源码插入导致的审阅行号已核对并更新。此处的通话测试是本地模拟 SIP 事务；并未证明 SIM World 真实通话长期保持。当前仍缺初始 INVITE 422 重试和对不支持 UPDATE 的对端改用 re-INVITE 的回退，后续需单独实现和验证。

本轮用隔离 Rust daemon 连接 ESTKme-RED，确认当前号卡 SIM World 454/03。VoWiFi 启动后公共状态短暂显示隧道 `connected`、IMS `registering`，随后变为隧道 `error`、IMS `failed`，公开错误为“IMS 注册网络传输失败”；`call_ready=false`、`sms_ready=false`。本轮没有取得新的 ESP 计数，不能把上一轮的 `8/0` 直接套到本次；此前 [Iteration 523](iteration-523-simworld-negotiated-child-sa.md) 的 `ESP 发出 8 / 收到 0` 仍是可用的上一次分层证据。问题尚未定位到 Surge 出口、ePDG 接收还是内层路由，IMS 注册、收短信及通话仍未验收。脱敏结果在 `src/build/reports/iteration-529-simworld-sanitized.json`；临时原始身份快照与隔离运行目录已删除，VoWiFi 已停止、读卡器已断开、daemon 已退出。本轮不切卡、不发短信、不拨号、不生成 DMG。
