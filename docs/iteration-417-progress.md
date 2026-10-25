# Iteration 417：移除 Go 本机 SDP 构造 fallback

生产 IMS 呼叫的本机 offer/answer 现在必须通过已接入 reader VoWiFi session 的 Rust `BuildLocalAudioSDP` callback 构造。删除了 Go `localSDPLegacy` SDP wire formatter；Rust builder 不可用时立即失败，不再静默由 Go 生成 SDP。已有 Rust 本地 SDP builder 对 endpoint、EVS/AMR-WB、payload type 和 telephone-event 参数执行校验。Go 仍整理本机调用事实、探测实际 EVS/AMR-WB codec 可用性并管理 codec/RTP/音频与 SIP session。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace 测试与格式检查、Go 全包 test/vet、Swift BLE proof/manager regressions、carrier/USB/radio/HTTP/SDP/AMR/MESSAGE parity 均通过；`go test -race ./internal/vowifi/imscore` 通过。契约扫描器 44 项测试、inventory `--diff` 通过；仍为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、370 reviewed scopes。

门禁日志：[dmg-preflight-iteration417-migration-gates.log](../build/optimization/dmg-preflight-iteration417-migration-gates.log)，SHA-256 `0f87b451354c731c5c1c705ee71044222398f6864518d4cdba42c8f0887823d3`。DMG 按用户本轮要求另外生成，发行记录见 [release-3.08-iteration417](release-3.08-iteration417.md)。未启动 App，未操作读卡器、SIM、短信、电话或运营商网络。

仅计入本机 SDP wire 构造这项已生产接管职责：P3 +0.025 至 **2.915 / 8.0（36.44%）**；项目累计 **17.620 / 47.5（37.09%）**，估值误差约 ±3 个百分点。结构退出审计仍有 7 类 Go 构建/运行/源码阻塞，生产 Go 源文件 264 个。本轮不代表语音或呼叫生命周期整体迁移完成。
