# Iteration 471：Rust 音频启动与通话模式变更互斥

日期：2026-10-13。为 Rust-owned 通话音频启动补齐跨进程 admission guard，避免它在 Go 正修改 USB/通话模式时读取 readiness 或执行 ADB 路由。

Rust 在 readiness 查询前对现有 `call-mode-operation.lock` 申请共享锁，并将 guard 移交给 detached voice-route worker；即使 HTTP 请求超时或取消，锁仍覆盖 ADB 完成和 USB VoiceTransitionLease 释放。Go 通话模式变更及 Rust 备份删除继续使用同一文件的独占锁，因此共享/独占双向互斥。音频 readiness Go 适配器现在只提供模块接口及运行时事实；Rust 持有 admission 决策。Go 拨号/接听仍检查原有进程内操作标志，Go 仍拥有通话模式硬件变更及独占锁，模块 readiness 数据来源也仍未迁出。

离线验证：共享锁与 Go/Rust 独占锁互斥测试通过；Rust daemon **1,477 passed / 0 failed / 3 ignored**；Go `proletarius-macos` 测试通过；完整 `scripts/test-migration-gates.sh` 通过，包含全 workspace Rust 测试、Go 全仓 vet/test、Swift BLE、2,726 项 carrier replay、USB/radio/HTTP method+HEAD 及 IMS/SDP/AMR/MESSAGE/USSD/dial-target/rejection parity。门禁日志 SHA-256：`ed2e02db858ce7eb7a6b0f6fbd267b1c9032c654bcca89d9cfa114ced7f6781c`。API contract `--diff` 与 `git diff --check` 通过，census 保持 355 method/path（Rust 295 / Go 60，含 51 HEAD aliases）、127 Swift calls、382 review scopes。

这是同一音频路由的并发安全边界补齐，没有将模块语音 readiness 事实或 call-mode 硬件执行迁入 Rust，因此不增加加权迁移比例，整体仍 **38.04%（18.070/47.5，估算误差约 ±3 个百分点）**。Go 退出仍有 6 类阻塞、266 个生产 Go 文件。全程离线，没有操作读卡器、SIM、短信、电话或运营商网络。本轮未生成 DMG；此前交付的 Iteration470 DMG 不包含 Iteration471 源码修改。
