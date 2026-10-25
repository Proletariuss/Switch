# Iteration 527：re-INVITE 拒绝响应重传一致性

2026-10-18。延续 [Iteration 526](iteration-526-offerless-reinvite.md)。Rust IMS 通话 owner 已按对话 CSeq 验证新 re-INVITE，但此前仅缓存 200 响应：若媒体提议被 400、415 或 488 拒绝，同一 Via/CSeq 事务重传会因为 CSeq 已消费而错误回复 500。网络重传不应改变这一事务的最终结果。

后端现在仅在拒绝响应成功写出后缓存该事务的最终响应。相同 Via 与 CSeq 的重传沿用原响应；下一次新 CSeq 仍独立处理。测试覆盖不支持的媒体编码返回 488、缺失 SDP Content-Type 返回 415，以及格式错误的 Contact 返回 400；每项重传保持状态码，原通话与 RTP 配置不变。缓存仍只保留最近一次完成的 re-INVITE 响应，更长历史的迟到重传需要后续事务层完善。

专项 20 项 IMS 通话测试通过；完整 Rust 工作区 **3044 通过、0 失败、1 忽略**。daemon 构建、`cargo fmt --check`、`git diff --check` 和 Rust-only 契约检查通过，HTTP owner 仍为 294/294。测试和构建日志位于 `build/reports/iteration-527-ims-tests-final.log`、`build/reports/iteration-527-rust-workspace.log`、`build/reports/iteration-527-rust-build.log`。本轮没有实卡会话、代理修改、发短信、拨号或生成 DMG；SIM World 的 ESP 入站为零和 IMS 未注册仍是独立的实网缺口。
