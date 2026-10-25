# Iteration 397：来电超时策略接入 Rust

2026-10-12

Rust `IncomingCallTracker` 现在只允许与当前待接 INVITE CSeq 匹配、且仍处于 Ringing 的事务进入超时终态。CSeq 不匹配、正在应答或已经应答的事务保持原状态。Go 保留 90 秒 timer 的调度和 SIP 408/500 回包执行；Rust 私有策略回调不可用时保留原兼容 fallback。该轮没有改变超时长度、SIP wire 响应或本地来电显示行为。

验证：定向 Rust tracker/route 与 Go bridge/runtime 测试通过；`scripts/test-migration-gates.sh` 全量通过，日志为 [`final-migration-gates.log`](../../build/optimization/final-migration-gates.log)，SHA-256 `b1703c4ec232c256b3988cf006b91d6772eb8a0e5f6ea47a1e7fc89fee5266c6`。契约 diff 与 43 项扫描器测试通过：346 method/path（Rust 286 / Go 60）、127 Swift 调用点、360 审阅范围。无实机通话/运营商验证。

P5 增加 0.025，累计 0.175 / 8.0；全项目 17.120 / 47.5 = 36.0421%（显示 36.0%，两位小数 36.04%，估算误差约 ±3 个百分点）。

本轮用户明确要求生成安装包，已生成并验证 [Switch 3.01 / build 301 DMG](../../dist/Switch-3.01-macOS27.dmg)。包包含 Go 与 Rust 两个后端进程，使用本机 ad-hoc 签名，未进行 Developer ID 公证；DMG 可读挂载、`hdiutil verify` 和签名检查通过，归档 SHA-256 一致。该包不代表全 Rust 目标完成。未操作读卡器、未发送短信或拨号。
