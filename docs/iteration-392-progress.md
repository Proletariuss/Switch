# Iteration 392：通话本地 SDP offer/answer 构造迁入 Rust

读卡器 VoWiFi 的实际呼叫建立路径现通过 Rust call codec 生成本机 SDP。Rust 决定 EVS/AMR-WB offer 顺序、EVS-only 互操作选项、telephone-event 属性以及 IPv4/IPv6 SDP 行格式；接听时按已协商的 EVS 或 AMR-WB mode、payload 和 header 格式构造 answer。Go 传递会话 ID、媒体端点和本地编解码能力事实，并保留 EVS 可用性探测、媒体 socket、编解码器实例和实时音频/RTP执行。

新增受应用 token 保护的 `POST /api/v2/call/build-local-sdp`，含 8 KiB 路由预算、4 KiB handler 限额、两秒读头/体期限和严格 JSON schema。Rust 返回需经 Go 校验；请求失败时生产通话不退回 Go SDP formatter。未改变 EVS 优先、AMR-WB 次选策略，AMR-NB 与 PCMA 仍不进入呼叫 SDP。

验证：Rust SDP crate tests、daemon call-codec route tests、Go imscore production callback test、Go Unix-socket app-token bridge test，以及 `scripts/test-migration-gates.sh` 全量离线门禁通过。迁移契约 `--diff` 通过：345 method/path contracts（Rust 285 / Go 60）、127 Swift 调用点、359 个审阅范围；43 项迁移契约提取器测试通过。门禁日志：[iteration-392-migration-gates.log](../build/optimization/iteration-392-migration-gates.log)，SHA256 `c2ee8abb2cfe4367ada54ddda2641a81a91c5b47f08f7b8fab928c82fe9a7611`。

本轮 P5 增加 **0.025 计划人周等值**；和 Iteration 391 的远端 SDP 解析/协商合计 **0.050 / 8.0（0.6%）**。项目累计 **16.995 / 47.5 = 35.7789%**，显示 **35.8%**（两位小数 **35.78%**，估算误差约 ±3 个百分点）。

Go 仍承担 SIP dialog/事务与 socket、RTP、音频 I/O、编解码器实例、物理 AKA、IPsec 及 VoWiFi 生命周期；此进度只代表 call setup 的 SDP 职责迁移。未操作实体卡、未拨打电话或发送短信、未做运营商/实机验收，未生成 DMG。
