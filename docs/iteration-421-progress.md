# Iteration 421：外呼挂断状态决策迁入 Rust

本轮把生产外呼的挂断选择接入已有 Rust INVITE tracker：`inviting/early` 决定发送 CANCEL，`active` 决定发送 BYE，`cancelling/terminating/ended/failed` 忽略重复操作。原先 Go 根据本地 CallState 直接选择 CANCEL/BYE 的策略分支不再用于外呼；Rust callback 缺失或输出不一致会返回错误且保持本地会话状态，不回退到 Go 判断。

Go 继续负责 live Call-ID 和状态复核、CSeq 分配、SIP 请求序列化与 socket 写入、CANCEL 后的响应 watcher、ACK/BYE 网络顺序和通话/媒体资源清理。专用控制 mutex 串行化用户 hangup 与最终 2xx 处理；ACK 已写入且本地 active 状态提交后才允许挂断路径发 BYE，避免并发时 BYE 抢在 ACK 前。入站呼叫和 SIP/媒体执行所有权不在本轮迁移范围内。

Rust tracker 37 项单测通过，daemon hangup 路由用例通过；Go `imscore` 与 `cmd/proletarius-macos` 测试及 race 测试通过。全量迁移门禁通过：[iteration421-migration-gates.log](../build/optimization/iteration421-migration-gates.log)，SHA-256 `58046281cc3afaace076d2ad695d51826ba7f3d46207cd1ab36234844548a437`。API census 仍为346 method/path（Rust286 / Go60）、127 Swift 调用点、370 review scopes；审阅及契约 `--diff` 通过。全 Rust 退出审计仍有7类 Go 构建、运行或源码依赖。

本轮计入 P5 +0.025 计划人周等值：累计 **0.300 / 8.0**；全项目 **17.720 / 47.5 = 37.31%**，估值误差约±3个百分点。没有读卡器、SIM、短信或运营商真实呼叫测试，也没有挂断/ACK 竞态实机验收；9eSIM `6982` 仍排除。当时未生成 DMG；之后按用户要求发布了 Iteration 421 包，现已由 Iteration 422 包更新。
