# Iteration 526：无 SDP re-INVITE 的应答与媒体切换

2026-10-18。沿用 [Iteration 525](iteration-525-in-dialog-reinvite.md) 的 Rust IMS 通话 owner。对于已经建立的通话，原实现把无 SDP 的 re-INVITE 直接回复 488，无法完成对端以 offerless INVITE 发起的会话刷新。本轮没有连接读卡器、修改代理、切卡、发短信、拨号或生成 DMG。

Rust owner 现在用当前已协商且媒体可用的编码、有效负载号和本地 RTP 端点，在 200 响应中发送 SDP offer。只有收到同连接、同 CSeq 的 ACK，且 ACK 带有 `application/sdp`、答案保持当前编码与有效负载号、媒体方向可用时，才按 ACK 中的新对端地址/端口更新 RTP。ACK 缺失 SDP 或答案不兼容时，发送 BYE、停止媒体并报告 `ims_reinvite_ack_sdp_invalid`；在此之前原媒体继续运行。已有的 32 秒 ACK 超时清理同样适用。

新增两项模拟对话测试：有效 ACK 答案在 200 后改变远端 RTP 端口；缺失答案触发 BYE 和媒体回收。专项 19 项 IMS 通话测试通过。完整 Rust 工作区 **3043 通过、0 失败、1 忽略**；daemon 构建、`cargo fmt --check`、`git diff --check` 和 Rust-only 契约清单通过，仍为 294/294 HTTP owner。日志见 `build/reports/iteration-526-ims-tests.log`、`build/reports/iteration-526-rust-workspace.log` 和 `build/reports/iteration-526-rust-build.log`。

构建前旧 Rust 增量缓存耗尽了本机磁盘空间；已仅清理 `Switch_MAC/build/rust-target/debug/incremental` 并关闭增量编译完成回归。源码、隔离运行数据和已安装应用未因缓存清理而变更。

限制：尚未实现本端主动发起 session refresh、`Session-Expires`/`Min-SE` 的完整计时协商、媒体 hold 或编码切换。此改动不触及 SIM World 的 ESP 发送和代理出口；先前实卡观察仍是 `esp_sent=8、esp_received=0`，因此不能宣称 SIM World IMS 注册、短信或通话验收完成。SIM World 入站为零仍须代理出口或 ePDG 的接收证据，或同卡同网已知正常客户端的对照。
