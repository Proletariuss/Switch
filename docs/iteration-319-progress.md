# Iteration 319：BLE compatibility lease expiry 清理迁入 Rust

删除 Go BLE compatibility lease 的 `time.AfterFunc` 和 timer 全局状态。Rust BLE handler 在 Go admission 成功后启动带同一 actor token 的 30 秒 deadline task；正常收到 Go release 确认时取消它，release 失败或 handler future 被丢弃时保留到原始 deadline，并向 Go 发送 token-bound release，最多尝试三次。Go 精确 token release 现在幂等，旧 token 仍不能清除后续新租约。Rust actor 的 BLE RAII permit 在 handler 结束/取消时释放自身 gate。Go 子进程随 Rust supervisor 生命周期终止并重启，其共享内存标志自然复位；Go 侧保留 token ownership、constant-time 比较和实时 session/N待 admission 检查。

定向验证：Rust `reader_ble_control` **7 passed**，新增 deadline 到期向 Go release 的 Unix-socket contract 测试；Go `TestReaderBLEOperationLease` **2 passed**，覆盖同 token 幂等释放及旧 token 不得清除新租约；Rust `cargo fmt --all -- --check` 及相关文件 `git diff --check` 通过。未运行全量迁移门禁。测试验证了到期调用和 token 对应，但未等满生产 30 秒，也未做进程崩溃/重启或实机 BLE 并发测试。

这一步把 Go 本地超时任务所有权迁入 Rust supervisor actor，Go BLE compatibility route 与共享状态仍保留，故 reader/Go removal 验收未通过。总加权进度暂维持 **31.4%**。未访问实体卡/运营商、未生成 App/DMG。
