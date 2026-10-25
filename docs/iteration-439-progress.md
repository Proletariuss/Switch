# Iteration 439：N 待定时任务协作式关闭

日期：2026-10-13

## 结果

Rust daemon 的 N 待维护通知与注册续期/重试调度器不再在关闭时被 `abort()` 强制取消。两项任务现在通过 watch 信号在空闲时立即退出；若一次有界、认证的 UDS 回调已经提交，则等待回调返回或达到原有 2 秒请求超时，再由 daemon 关闭 callback/internal listener task 集合。Rust 对 durable due 事件仍只在收到成功回调后确认，原有 Go 会话执行适配器、计时和重试策略保持不变。

这是 P1 生命周期可靠性加固，不迁移新的生产业务 owner，也不宣称 Rust VoWiFi/SMS session 已接管，因此加权进度仍为 **37.99%（18.045/47.5，估算误差约 ±3 个百分点）**。契约 census 保持 354（Rust 293 / Go 61）、127 个 Swift 调用点、378 个审阅范围；Go 退出审计仍有 6 类阻塞和 266 个生产 Go 源文件。

## 验证

- 两项定向退出测试通过，分别验证 idle maintenance/retry scheduler 能在收到 watch 关闭信号后及时退出。
- `scripts/test-migration-gates.sh` 完整通过：Rust workspace 中 `proletariusd` **1447 passed / 3 ignored**；Go 全仓测试与 vet、Swift BLE 回归、2722 项 carrier replay，以及现有 USB/radio/HTTP/HEAD/IMS/codec 差分通过。
- `cargo check --locked -p proletariusd` 与 `cargo fmt --all --check` 通过。
- 迁移契约清单更新并通过 `scripts/check-rust-migration-contracts.py --diff`；路由、consumer 和 ownership 计数没有变化。
- Rust-only 退出审计仍被 Xcode 构建 Go、App 嵌入 Go binary、Rust 启动/分发到 Go、Go module 和 266 个 Go 生产源阻塞；本轮没有减少这些阻塞。
- 未连接读卡器、SIM 或运营商网络；没有真实短信/VoWiFi 验收。9eSIM `6982` 仍排除。
- 此轮未生成 DMG。刚生成的 3.15/build315 DMG 基于 Iteration 438 源码，不包含 Iteration 439。

## 后续

下一步继续从真实 Rust IMS transport 与 keyed session actor 的接线入手，完成单 Profile 生命周期和受保护 SMS 命令分派；readiness 仍保持关闭，直到相同 reader lease、网络 owner、IMS actor 与清理证明形成一个可离线故障验证的生产闭环。
