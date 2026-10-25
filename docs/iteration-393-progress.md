# Iteration 393：外呼 INVITE 状态机接入 Rust

读卡器 VoWiFi 的生产外呼路径现会在发送 INVITE 前向 Rust call tracker 注册 Call-ID/CSeq，并把收到的 provisional/final response 事实交由 Rust 决策。Rust 负责 phase 转换、可靠 provisional 的 RSeq 顺序、早期媒体准入、CANCEL 竞争以及最终 ACK/BYE 动作。Go 仍负责 SIP 响应分发、请求序列化与写入、dialog 和媒体执行、通话清理；本轮迁移的是状态机与策略切片，不是 Rust 端到端通话接管。

Go/Rust 信任边界现在会拒绝与 SIP 状态类别不符的动作、缺少可靠 RSeq 的 `send_prack`、不合法的早期媒体标志，以及失败状态码与收到响应不一致的 `ack_fail`。Rust bridge 失败时，Go 仍执行必要协议清理：对进行中的 provisional 事务发送 CANCEL，对已建立的 2xx dialog 先 ACK/BYE，再结束本地通话。现有 transport/session 与媒体 owner 未改变。

离线验证通过：

- Go 定向测试覆盖生产 Unix socket/app token bridge、动作与状态码校验、可靠 provisional 的 PRACK 构造、最终非 2xx ACK 和回调生命周期顺序。
- Rust daemon 路由测试覆盖可靠 183 后接 200、重复最终响应 ACK、取消后的 487 结束、重复 begin 拒绝及 INVITE CSeq 不匹配。
- `scripts/test-migration-gates.sh` 通过：Rust 格式与 workspace 测试、Go `vet` 和全量测试、Swift BLE 操作完成证明/管理器回归，以及 carrier、USB 状态、模块 radio、HTTP method/HEAD、SDP、AMR 和 MESSAGE 差分检查。完整日志：[iteration-393-migration-gates.log](../build/optimization/iteration-393-migration-gates.log)，SHA256 `ec12efe42d016b17af100f16e8566d479e95d2ad492338b03f409df163cf2470`。
- 迁移契约扫描确认 346 method/path（Rust 286 / Go 60）、127 Swift HTTP 调用点、359 个指纹化审阅范围，与更新后的库存一致；迁移契约审计自测 43 项全部通过。日志：[`iteration-393-contract-diff.log`](../build/optimization/iteration-393-contract-diff.log)、[`iteration-393-contract-tests.log`](../build/optimization/iteration-393-contract-tests.log)。
- `git diff --check` 通过。

本轮 P5 增加 **0.025 计划人周等值**，Iteration 391–393 累计 **0.075 / 8.0（0.9%）**。项目进度为 **17.020 / 47.5 = 35.8316%**（显示 35.8%，两位小数 35.83%，估算误差约 ±3 个百分点）。迁移尚未完成；Go 仍拥有大部分 SIP/媒体执行、IMS/IKE 生命周期、设备交互和应用后端功能。

本轮没有操作实体读卡器/卡片、收发短信或拨打电话，因此没有实机或运营商验收证据。用户要求的 2.99 DMG 是本轮早些时候从当时源码生成的；它早于本轮最后补充的跨边界动作校验，不代表当前工作树完整状态。没有为本轮额外生成 DMG。
