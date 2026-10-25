# Iteration 414：外呼 INVITE 策略强制由 Rust 裁决

外呼拨号在 Rust INVITE 策略 callback 可用前失败关闭；生产 Go 不再通过兼容分支自行裁决 phase、早期媒体或 PRACK/ACK/BYE action。移除了旧 provisional-state Go 判定 helper，并新增缺少 Rust policy 时呼叫保持 idle、没有分配媒体的检查。Go 继续负责 SIP 清理写入、媒体执行和 session 生命周期。

完整迁移门禁通过：Rust workspace、Go 全包测试与 vet、Swift BLE 回归及 Rust/Go 号码与错误映射 parity 均通过；API 契约 `--diff` 通过，census 为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、370 审阅范围。门禁日志：[iteration-414-migration-gates.log](../build/optimization/iteration-414-migration-gates.log)，SHA-256 `71443c2ab04d4f20a909f4132759691fb96d2966d5e2d124bd2f233610442524`。

P3 +0.025 至 2.840/8.0；项目合计 17.545/47.5（36.94%，估算误差约 ±3 个百分点）。全 Rust 退出仍阻塞于 7 类 Go 构建、运行及源码依赖；没有运营商实机通话验收。

已按本轮源码构建 3.08/build308 DMG：[Switch-3.08-macOS27-iteration414.dmg](../dist/Switch-3.08-macOS27-iteration414.dmg)。应用签名验证通过，`hdiutil verify` 校验通过。构建日志：[dmg-3.08-iteration414-build.log](../build/optimization/dmg-3.08-iteration414-build.log)，SHA-256 `d15678d722b7bc3c84eb7044b7663b2a36d188d16c77ceea57710cbfd57f30ce`。
