# Iteration 525：已建立通话的 SIP re-INVITE

2026-10-18。Rust IMS 通话 owner 此前把带有当前 Call-ID 和双方 tag 的 re-INVITE 当成第二通来电，在通话进行中回复 `486 Busy Here`。这一请求实际属于现有对话；对端用它刷新或修改会话时，误拒绝可能导致通话提前中断。

现在先识别同一对话的 INVITE。对于已连接、带 SDP 且使用当前语音编码的提议，后端返回包含递增本地 SDP 版本的应答；在匹配的 ACK 到来前，保持原 RTP 目标，ACK 后才更新媒体与 Contact 目标。相同事务重传重用已发送的 200；有待 ACK 的并发 INVITE 返回 491；ACK 逾期则发送 BYE 并关闭媒体。无 SDP、无法处理的编码/媒体方向及畸形 Contact 被明确拒绝，拒绝时保留原通话。

新增用例覆盖主叫与被叫建立通话后的 re-INVITE、200 重传、ACK 前后媒体目标、拒绝后通话保持和 ACK 超时清理。该改动不改变 SIM World 的 IKE/ESP 路径，也没有在本轮进行实卡通话。完整 session timer、无 SDP re-INVITE、主动发起刷新、媒体保持和编码切换仍待实现；不能因此宣称运营商通话验收完成。

验证记录：`cargo +1.98.0 test --locked --workspace -- --test-threads=4` 共 **3041 通过、0 失败、1 忽略**；`cargo +1.98.0 build --locked -p proletariusd`、`cargo fmt --all -- --check`、`git diff --check` 通过。迁移清单只有本文件新增一行导致的 3 个源码行号变化，人工核对后刷新清单；契约仍为 294/294 Rust HTTP owner。日志见 `build/reports/iteration-525-rust-workspace-rerun.log` 和 `build/reports/iteration-525-rust-build.log`。首次全量并行测试中，一项旧 VoWiFi CHILD rekey 集成用例失败，独立重跑通过；本轮无 DMG。
