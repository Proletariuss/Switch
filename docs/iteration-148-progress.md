# Iteration 148：读卡器列表 Go 依赖审计补正

日期：2026-10-08。变更及测试材料只写入 `/Users/proletarius/Desktop/Switch/`；未生成 DMG，未访问模块、读卡器、SIM、真实短信或外网。

复核 Rust `GET /api/reader/list` 时发现契约清单把 `go_dependencies` 留空，但 `reader_inventory::reconcile` 会在 Rust 扫描 PC/SC 后调用 Go 私有 `POST /api/internal/reader/reconcile`，以获取 Go `readerManager` 持有的选卡和 BLE 在途状态。该状态还与 VoWiFi、读卡操作及 hotplug 生命周期相关，不能通过移除私有 RPC 或使用 Rust 的旧选择提示来宣称完成迁移。

现已在 `api-contract-review.json` 明确登记这一依赖，刷新 `api-contract-inventory.json` 后，`GET /api/reader/list` 及其 HEAD alias 都记录同一 Go 依赖。新增扫描器回归用例检查人工审阅依赖被应用到 Rust owner 路由并传播到 HEAD alias；清单说明同步更新当前 method/path、owner、Swift 调用点和 scope 数量。此修正提高 P0 清单准确性，不改变 handler owner、生产运行路径或迁移进度百分比。

验证：契约工具自测 **43项通过**；普通 census 通过并与清单一致：279条 method/path（Rust205、Go74，含33个 HEAD alias）、127个 Swift HTTP 调用点、228个审阅范围。Rust-only 门禁按预期仍失败：默认 Go fallback 存在，141条路由仍有 Go owner/dependency。检查日志：[contract-tests.log](../build/optimization/iteration-148/contract-tests.log)、[contract-census.log](../build/optimization/iteration-148/contract-census.log)、[rust-only-gate.log](../build/optimization/iteration-148/rust-only-gate.log)。

总体估算保持 **27.8%**（P2A 68.36%，P2B 59.06%；约±3个百分点）。本轮不计新迁移人周。下一阶段应把 readerManager 选择状态、BLE身份观察、PC/SC presence、共享租约和 VoWiFi/hotplug 生命周期作为同一资源 owner 迁移，保持 reader reconcile 的非破坏性轮询语义。
