# Iteration 329：清理已退役的 Go reader/reconcile 实现

2026-10-10。代码和验证材料仅在 `/Users/proletarius/Desktop/Switch`。未访问读卡器、SIM、运营商或短信；未构建 App/DMG。

## 变更

- 删除 Go `readerReconcile` handler 及它独有的请求验证/读卡器列表拼装函数；对应私有路由已在 Iteration 328 注销，因此没有公开或内部 API 变化。
- 删除只验证该退役 Go handler 的测试。Rust reader inventory、Swift BLE status 与 Rust hotplug reducer 测试继续覆盖当前生产实现；Go reader connect/disconnect 和物理 teardown 测试保留。
- 迁移清单仍为 **339 method/path（Rust 278 / Go 61）**、127 Swift 调用点和 354 审阅范围。数量不变是预期结果：路由归属已经在 Iteration 328 迁移，本轮仅清理旧实现。
- 不重复计算 Iteration 328 的生产职责迁移分。全 Rust 加权进度保持 **15.395 / 47.5 = 32.41%（约 32.4%）**。

## 验证

- Go reader 定向回归：`go test ./cmd/proletarius-macos -run 'TestReader' -count=1` 通过。
- 全量 `scripts/test-migration-gates.sh` 通过：daemon **1,316 passed / 3 ignored**，Go 全仓 test/vet、Swift BLE physical proof 与 38 组 manager event sequences、2,722 carrier cases、USB/radio/HTTP/codec/SDP/MESSAGE parity 均通过；日志：[Iteration 329 migration gates](../build/optimization/iteration-329-migration-gates.log)。
- 契约盘点通过：339 个 method/path；契约扫描器 43 项自测通过；`git diff --check` 通过。

## 仍未完成

Go 仍负责物理 reader disconnect、VoWiFi/N待失效清理、reader connect/rebuild、身份初始化和 pin/finalize 适配；IMS、短信、N待、模块与通话运行时仍未全面切换到 Rust。此次源码清理不代表硬件拔插或在线业务验收通过。
