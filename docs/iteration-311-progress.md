# Iteration 311：读卡 worker 准入按 reader 隔离

Iteration 310 的共享 Gate 已保护 N待 reservation 与 Rust 原生 AKA/身份读卡 worker，但 worker admission 使用全局计数，导致一个读卡器上的任务可能阻塞另一个读卡器。

本轮将 worker permit 改为按物理 reader key 计数。`reader_task` / `reader_aka_binding::run_owned` 接收显式 reader 标识；身份读取、EAP-AKA、普通 AKA、PC/SC session lease 与 BLE AKA 都以实际选中的 reader admission。N待仍保持进程内单一 owner；它只与同一物理 reader 上的 worker 互斥。未绑定 reader 的内部保守 acquire 仍会检查全部活跃 worker；release 只等待其绑定 reader 的 worker，避免无关 reader 阻止清理。worker permit 依旧存放在 blocking closure 内，直到真实物理任务退出；超时或取消等待者不会提前释放。

验证：`cargo +1.98.0 fmt --all -- --check` 通过；`cargo +1.98.0 test -p proletariusd` 通过（1200 passed、3 ignored）；完整 `scripts/test-migration-gates.sh` 通过，包括 workspace Rust 测试、Go vet/测试及 carrier/USB/radio/HTTP method+HEAD/codec/SDP/AMR/IMS MESSAGE parity。API census 仍是 334 个 method/path（Rust 273、Go 61）、127 Swift HTTP 调用点、327 个审阅范围；scanner tests 与 `git diff --check` 通过。

边界：本轮只精细化 Rust 进程内 Gate 的 worker admission，不接管直接 Go APDU 消费者，也不移除 Go `readerManager.authMu`、BLE watchdog、reader lease lifecycle 或业务 teardown。没有使用真实读卡器、发送短信、拨号或生成 DMG；全 Rust 后端迁移目标继续推进，整体加权进度不变。
