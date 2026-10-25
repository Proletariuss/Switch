# Iteration 315：reader gate 承接 BLE 操作的迁移边界复核

本轮复核 `reader_ble_control`、Go `readerBLEOperationLease` 与 Rust `reader_reservation::Gate`。Rust gate 已支持按物理 reader 与 generation 绑定操作租约，并与 Rust blocking worker、N待 reservation 互斥；BLE 公共路由当前仍向 Go `/api/internal/reader/ble-operation` 申请临时租约。Go 租约同时检查 `readerVoWiFiOperationActive`、`readerNativeSessionLeaseActive` 与 `readerMultiStandbyActive`，这些状态仍由 Go 的共享互斥量维护。源码搜索约有 112 处匹配，包含测试和字段读写，不能当作 112 个生产调用点。因此只把 BLE 租约改接 Rust gate 会丢失与未迁移 Go session/操作的互斥，当前不能安全删除 Go 适配器。

定向验证：`cargo test --manifest-path rust/Cargo.toml -p proletariusd --bin switchd-rs reader_reservation::` 通过，8 passed、0 failed。它验证了绑定校验、reader/generation/token 约束、reservation/worker 原子互斥、同 reader worker 存活时不释放以及 Go 子进程确认退出后的外部租约清理；没有覆盖跨进程 Go VoWiFi 与 Swift BLE 的真实并发，因此不构成 BLE owner cutover 证据。

本轮未修改运行时代码，未运行完整迁移门禁，未访问实体卡/运营商或生成 DMG。总进度维持 **31.4%**。下一步应先由 AST/调用点测试准确清点生产 acquire/release，再把 Go 的共享 reader-operation state（VoWiFi operation、native session 与 N待状态）纳入 Rust reader actor 的统一租约协议，并完成双端并发测试；之后才能切换 BLE 控制租约、删除 Go token/timer 和对应 internal route。
