# Iteration 316：共享 reader-operation 状态调用点清单门禁

新增 Go AST 回归测试 `backend/cmd/proletarius-macos/reader_operation_owner_inventory_test.go`，扫描非测试源码并锁定共享状态的生产 writer 文件集合：BLE 操作标志由 `reader_ble_api.go`、`reader_vowifi_api.go` 写入；native session 标志由 `reader_vowifi_api.go` 写入；N待标志由 `multistandby_manager.go` 写入。它也锁定直接访问文件集合：VoWiFi operation 状态在 `multistandby_manager.go`、`reader_ble_api.go`、`reader_vowifi_api.go`；native session 状态同样在这三处；N待状态还被 `reader_manager.go` 直接读取。`reader_manager.go` 对 VoWiFi operation busy 的判断经 `bleOperationBusy()` 间接完成，不会伪计为变量直接访问。测试同时锁定 `begin/endReaderVoWiFiOperation` 的生产调用文件（`multistandby_manager.go`、`reader_api.go`、`reader_esim_api.go`、`reader_session_lease.go`、`reader_vowifi_api.go`），以及 native lease window/transfer 当前仅由 `reader_session_lease.go` 调用。新增或迁移 writer/caller/accessor 会要求显式更新清单，减少 actor 切换漏项。

验证：`go test ./cmd/proletarius-macos -run '^TestReaderOperationOwnerProductionInventory$' -v` 通过。将直接访问文件初值按 grep 推断时测试失败，AST 输出显示了 BLE 租约处理器与 N待状态的实际直接访问，修正清单后再通过。Rust `reader_reservation` 的 8 项定向测试也在 Iteration 315 已通过；这些离线证据不等于 Go/Rust 跨进程 actor 已切换。

本轮只新增迁移调用点门禁，没有改运行时所有权、没有运行全量迁移门禁；加权进度仍为 **31.4%**。未操作读卡器/SIM、未访问运营商、未构建 App/DMG。下一项应按此白名单把 Go operation-state 转为 Rust reader actor 调用，并在每个迁移批次更新清单与进行 Go↔Rust 并发差分。
