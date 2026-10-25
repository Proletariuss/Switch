# Iteration 318：统一 BLE 控制租约 token 来源

BLE 控制请求的 256-bit lowercase-hex token 由 Rust reader actor 生成；Rust 将同一个 token 交给 Go compatibility admission adapter，并校验 Go 回传 token 与 actor token 完全相同。Go 不再独立生成第二个随机 token，避免 Rust permit 与 Go compatibility lease 出现不同 owner ID。Go 仍保留 token 比对和 30 秒 timer 作为旧 VoWiFi/N待互斥适配及释放 RPC 丢失的自恢复；它们尚未迁出。

验证：Go `go test ./cmd/proletarius-macos -run 'TestReaderBLEOperationLease' -v` 两项通过。Rust `cargo test --manifest-path rust/Cargo.toml -p proletariusd --bin switchd-rs reader_ble_control::` 6 项通过（新增 Go RPC 使用原 token及拒绝错配 token）；`cargo fmt --all` 已运行。未运行全量迁移门禁。

这一改动统一了跨进程 BLE 租约的身份来源，但 Go 仍存储 BLE lease/token/timer 与全局状态，`reader_ble_controls` 仍依赖 Go，整体进度仍按 **31.4%** 记录。未操作实体卡、未访问运营商、未生成 App/DMG。后续需迁移 Go fallback state 与超时清理，并证明 Go-owned live VoWiFi/N待状态已被 Rust actor 完整观测后，才可删除 Go compatibility route。
