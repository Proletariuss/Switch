# Iteration 475：通话模式重启拒绝判定归 Rust radio owner

Rust `module_radio_transport::Plan` 现在识别重启命令应答中精确的 `ERROR`、`+CME ERROR:` 或 `+CMS ERROR:` 行，即使其后还有终止 `OK`，也返回有明确错误码的 `radio_reboot_rejected`。启用通话模式和恢复备份的 Go 工作流不再解析原始 AT 应答，只消费该结构化错误码。传输超时、响应不完整或结果不确定仍沿既有重连验证路径处理；未改变模组断连后继续确认状态的语义。

离线验收：Rust radio reboot 专项 **1 passed**；Go call-mode 定向测试通过；完整 `scripts/test-migration-gates.sh` 通过，Rust daemon **1,482 passed / 0 failed / 3 ignored**，Go 全仓 test/vet、Swift BLE 虚拟管理器回归、2,726 项 carrier replay、USB/radio/HTTP/HEAD/IMS/SDP/AMR/MESSAGE/USSD 与拨号差分通过。Rustfmt、迁移契约 `--diff`、`git diff --check` 通过。契约 census 为 354（Rust295/Go59，含51个 HEAD aliases）、127 Swift调用点、381 fingerprinted review scopes。Go 结构退出审计仍为6类阻塞、265个生产Go源码；完整 call-mode reservation、watchdog、部署及恢复仍在 Go。

本切片计入 P2B **+0.025 计划人周等值**：P2B **4.975/8.0（62.19%）**；总体 **18.145/47.5=38.20%（误差约±3个百分点）**。无实机、读卡器、号卡或运营商验收，9eSIM `6982` 继续排除。本轮代码在之前用户要求生成的 Iteration474 DMG 之后修改，因此该 DMG 不含本轮变更；本轮没有重新生成 DMG。
