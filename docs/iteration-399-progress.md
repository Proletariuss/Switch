# Iteration 399：PRACK/UPDATE SDP 验证切换至 Rust

2026-10-12

入站 PRACK/UPDATE 携带 `application/sdp` 时，Go 过去通过临时 `callRuntime` 直接走旧 SDP parser。现在候选 SDP 复用生产配置的 Rust SDP negotiator；candidate 仍与实时会话隔离，只在 wire 层保留原行为：解析/端点/codec 不兼容仍回 488，当前 codec 匹配则回 200，不会把未协商的候选媒体参数写进活动通话。用户真实音频状态、RTP socket 和 SIP responder 继续由现有 owner 管理；没有启用 re-INVITE/媒体重协商。

验证：4 组 PRACK/UPDATE × EVS/AMR-WB 兼容测试通过，另有定向 Go SDP 测试、Rust call tracker/codec workspace tests、契约 `--diff` 与 43 项扫描器自测通过。完整门禁日志 [`iteration-399-migration-gates.log`](../../build/optimization/iteration-399-migration-gates.log) SHA-256：`e111aa0b4115144d3a8b55a94088cf8457278991fa71f41aee2b312806f03a2b`。契约仍为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、360 审阅范围。无实机或运营商通话验收。

P5 增加 0.025，累计 0.225 / 8.0；全项目累计 17.170 / 47.5 = 36.1474%（显示 36.1%，两位小数 36.15%，估算误差约 ±3 个百分点）。

本轮实施完成时尚未生成 DMG。随后用户明确要求打包，已生成 [3.02 / build 302 DMG](../../dist/Switch-3.02-macOS27.dmg)，包含本轮 Iteration 399 改动；发行校验见[3.02 发布记录](release-3.02.md)。
