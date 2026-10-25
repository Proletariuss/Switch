# Iteration 307：读卡器 eSIM 健康探测移除 no-eUICC 公共 Go 回退

Rust 已通过 generation-bound reader lease 在 PC/SC 或 BLE 传输上完成已知 ISD-R AID 健康扫描，但 `NoEuicc` 结果此前仍将整个公开请求转发给 Go。核实 Go `readerESIMHealth`：读卡器 `NewManager` 失败对应 `503 reader_esim_unavailable`；manager 可用但所选 CCID/BLE channel 无可用 ISD-R 时，`isPhysicalSIMESIMProbeError` 不成立（它必须包含 `AT+CCHO ... ERROR`），返回 `502 reader_esim_health_failed`。reader manager 的选路只创建 PC/SC 或 BLE channel，不走模块 AT+CCHO。

为了让 Rust 自己保持相同响应，Go 私有 reader-session lease 快照新增 `esim_manager_available` 事实，取值在 reader identity/generation 锁内读取。Rust处理：`false` 时先以确认释放 lease，再返回原 503；`true` 且 AID 扫描结果为 `NoEuicc` 时直接返回原 502，不再调用 Go public handler；旧 Go bridge 未提供该可选字段时释放 lease 后继续走原兼容回退。保留了未选中/lease失败、桥预检失败等尚需原 Go 行为的回退。

Go lease测试覆盖 manager true/false；Rust lease DTO测试覆盖 true/false/legacy-missing；Rust health response测试校验 502 status/code/message/retryable。修改后完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,194 passed / 3 ignored**，Go全仓 test/vet及 carrier、USB、radio、HTTP method/HEAD、历史 codec helper、SDP、AMR、IMS MESSAGE parity通过。Go 1.26.3在线下载失败，使用Switch/build临时shim调用本机Go 1.26.7，测试后清理。

这是一个有界公开错误/回退分支接管，不等于reader manager、lease、Profile/APDU或PC/SC/BLE owner全Rust化；整体加权进度仍记为31.4%，9eSIM 6982仍排除。没有实机读卡、短信、通话、运营商访问或DMG。
