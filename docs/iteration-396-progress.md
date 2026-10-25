# Iteration 396：入站 CANCEL 判定接入 Rust

Rust `IncomingCallTracker` 现在以 Call-ID 下的原 INVITE CSeq 与呼叫阶段决定入站 CANCEL 是否仍有效。匹配且尚处于 Trying/Ringing 时接受并将 tracker 结束；CSeq 不匹配、无 tracker 或呼叫已进入 Connecting/Active 时拒绝。Go 消费该决定：有效 CANCEL 保持原有 CANCEL 200 后给 INVITE 发送 487 的顺序；无效/过期请求发送 481。Go 仍拥有 SIP responder、`incomingFinalMu` 序列化、通话状态/媒体和最终清理。

验证：Rust `proletarius-call` 31 项、daemon 呼叫策略 10 项、Go imscore/native bridge 定向用例通过；全量 `scripts/test-migration-gates.sh` 通过：[门禁日志](../build/optimization/iteration-396-migration-gates.log)，SHA-256 `5b7667ea904dd4457d1a05e37247b1ab2c669a62031e3aac1e26e18f7f0d0bcc`。迁移契约 diff [日志](../build/optimization/iteration-396-contract-diff.log)、清单更新和 43 项审计测试[日志](../build/optimization/iteration-396-contract-tests.log)通过；仍为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、360 审阅范围。`cargo fmt --all --check` 与 `git diff --check` 通过。

P5 +0.025，累计 **0.150 / 8.0**；项目累计 **17.095 / 47.5 = 35.9895%**（显示 36.0%，两位小数 35.99%，估算误差约 ±3 个百分点）。本轮没有读卡器或运营商实机呼入/取消竞争验收，也没有生成 DMG。Release `.app` 使用 `scripts/build.sh` 构建成功并通过严格签名验证：[构建日志](../build/iteration-396-app-build.log)，SHA-256 `e8e050bf1937a61c204146998ec8dc4c23c2ff6e3b951af16da385963297cad0`；包内版本 3.00/300，并仍包含 Go `switchd` 与 Rust `switchd-rs`，所以全 Rust/零 Go 目标尚未达到。已发布的 3.00/300 DMG 包含 Iteration 395，不包含本轮源码；9eSIM `6982` 仍排除。
