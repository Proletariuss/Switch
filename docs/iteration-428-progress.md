# Iteration 428：移除 Swift 独立 Go 启动入口

`BackendProcess` 现在固定启动 `switchd-rs` 并固定探测 Rust 网关 `/api/v2/runtime`；`SwitchBackendMode=go-compatibility` 不再能切换到独立 Go 可执行文件或旧 `/api/health` 探测。Rust 网关仍会监督 Go 硬件后端，因此本轮缩小了 Go 启动面，但没有让正常运行脱离 Go，也没有完成 Go 二进制的构建/嵌入移除。

验证通过：`scripts/test-backend-readiness.sh` 覆盖启动、认证健康检查、Rust gateway 与 Go 硬件 readiness 分离、重试/退出/取消竞态；`scripts/tests/test_rust_backend_exit.py` 4 项通过；`scripts/check-rust-backend-exit.py` 从 7 类降为 6 类，消除 `swift-go-compatibility-mode`，剩余 Xcode Go 构建、Go 二进制嵌入、Rust 监督/转发 Go、Go 模块和 264 个生产 Go 源文件；Rust API 契约 `--diff` 通过，仍为 349 method/path（Rust 290 / Go 59）、127 Swift 调用点、371 审阅范围。完整迁移门禁：[iteration428-migration-gates.log](../build/optimization/iteration428-migration-gates.log)，Rust workspace 1429 passed / 3 ignored，Go 全包测试与 vet、Swift BLE 和跨语言 parity 均通过。门禁 SHA-256：`e775664236f6af498171d80f1895119b9afe5f3896879a4cd508608da418f1cc`。Go 退出快照：[iteration428-go-exit.txt](../build/optimization/iteration428-go-exit.txt)，SHA-256：`6870fbb1f64df1fc7533b798b5d80bc23cda475102e6cf3d1bd56d1be267457a`。

加权进度保持 **37.62%（17.870 / 47.5）**。这次只删除冗余启动入口，正常后端功能仍依赖 Go，因此不增加功能迁移分。没有设备/运营商实机验收，也没有生成 DMG；最新 DMG 仍是 3.10/build310 Iteration 426。
