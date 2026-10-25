# Iteration 474：运行时下载进度由 Rust 持有

日期：2026-10-13。通话模式运行时下载原已由 Rust 执行文件请求、SHA-256 校验、断点缓存与下载线程管理，但 Go 仍逐次接收并保存下载中的字节数/详情。本轮改为 Rust 下载 job 作为活跃进度唯一来源：`GET /api/call-mode/status` 在先验证 Go 的活动操作快照后，仅当状态仍为 downloading 且下载来源、runtime version、总大小完全匹配时，叠加 Rust job 的 byte/detail。错配、非下载和终态快照保持原响应不变。Rust 只向 Go 发送完成/失败终态；Go 拒绝中间进度回写，继续负责模块/USB准入、操作 reservation/watchdog、完成后的运行时部署与失败收尾。

离线验收：Rust 下载器专项 **14 passed**，call-mode status 公共代理专项 **1 passed**，Go 后端 package tests 通过；完整 `scripts/test-migration-gates.sh` 通过，Rust workspace **1,481 passed / 0 failed / 3 ignored**，Go 全仓 test/vet、Swift BLE、carrier replay、USB/radio/HTTP method+HEAD、IMS/SDP/AMR/MESSAGE/USSD/dial-target/rejection parity 均通过。Rustfmt、合同 `--diff`、`git diff --check` 通过。合同 census 354（Rust295/Go59，含51 HEAD aliases）、127 Swift HTTP调用点、381 fingerprinted review scopes。门禁 SHA-256：`e65b2fed495e63a7cf3dfd4a15e643df0f98484de142240ec40e39ce584b0af9`；合同差异 SHA-256：`fb195042cf70ee9a037e9f7defac9ea343ee1cd75e16505ee30b228924494c49`。

本切片将 P2B 计 **+0.025 计划人周等值**：P2B **4.950/8.0（61.88%）**，总体 **18.120/47.5 = 38.15%（误差约±3个百分点）**。这不是整个运行时下载生命周期或通话模式的 Go 退出；Go 仍实施硬件准入和模块部署，整体结构退出审计仍有6类阻塞、265个生产Go源文件。未操作读卡器、SIM、短信、通话或运营商网络；未生成 DMG。9eSIM `6982` 继续排除。门禁日志：[完整迁移门禁](../build/reports/iteration-474-migration-gates.log)、[契约差异](../build/reports/iteration-474-contract-diff.log)、[Go退出审计](../build/reports/iteration-474-go-exit-audit.log)。


After Iteration 474, at the user's explicit request, the same source was packaged as [Switch-3.15-macOS27-iteration474-20261013.dmg](../dist/Switch-3.15-macOS27-iteration474-20261013.dmg), SHA-256 `7de616190e794d4ff81c52e5dacf783fcbdfb55fa6bb9f2109262639f08361f8`; `hdiutil verify` passed. This artifact predates Iteration 475.
