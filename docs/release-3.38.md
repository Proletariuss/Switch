# Switch 3.38（构建 338）

2026-10-14。按用户要求，从 Iteration 499 当前源码构建。

[下载 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.38-macOS27.dmg)。Apple Silicon，macOS 27.0+。

- 修复对端 CHILD SA 换钥后旧会话未退休阻塞后续换钥的问题，保留冻结重传与交叉 DELETE 的确认语义。
- AUTH_LIFETIME 使用独立绝对到期时间；零值触发真实清理和重新鉴权。修复正常停止心跳被误判为读卡器丢失，并保留真实丢失与计划续期同时发生时的正确优先级。
- 删除已不再使用的 Go 普通读卡器执行代码。新的模块 Rust Service、卡片与短信适配仍未启用公开接管，现有模块运行入口保留。
- 最终 Rust workspace：2,759 passed / 0 failed / 7 ignored；其中 4 个差分夹具已在完整门禁显式执行通过，其余 3 个互操作夹具未在该门禁执行。Go vet/test、Swift BLE、13 组协议差分、49 项扫描器自测及 320 条接口契约核验通过。
- 实际本机模拟协议组合覆盖双轮换钥、丢失 ACK 重传、交叉 DELETE、双向短信、AUTH_LIFETIME 到期后的新 IKE/鉴权/注册，以及 EVS 通话跨换钥的 RTP/PCM 和历史记录。
- Release 构建、严格签名、DMG 校验、只读挂载与构建 App 的逐文件一致性检查通过；4 个 Mach-O 全部 arm64，13 份许可齐全。独立发行审阅通过，两个后端完成不初始化硬件的装载检查。

本机 ad-hoc 签名，未公证；没有安装替换或开展本版实卡、运营商、真实音频和长稳验收。仍包含 Go 后端，全 Rust 目标未完成。

SHA-256：`40314f1365bc828e2e8cc2c3519079fb1a0f5c510fc23c21d83be0732db16533`

大小：55,543,972 字节。

[本轮源码记录](iteration-499-progress.md)。验证证据：`../build/reports/release-3.38/` 和 `../build/reports/iteration-499-*`。
