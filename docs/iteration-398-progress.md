# Iteration 398：入站 BYE 准入迁入 Rust

2026-10-12

入站 BYE 现在会先经过 Rust 来电事务 tracker。Rust 用受跟踪 INVITE 的 Call-ID 和原始 CSeq 标识当前来电事务，只对仍存活的来电阶段批准终止；拒绝旧/已结束事务。Go 保留当前 runtime 资源实例检查、SIP 200/481/500 回包、媒体与会话清理。BYE 自身 CSeq 与原 INVITE CSeq 不同，因此传给 Rust 的 CSeq 是 tracker 的原 INVITE 事务键，未改变 wire CSeq 或 SIP 报文。

验证：Rust call tracker 33 项测试、daemon call codec 定向测试、Go imscore/runtime 与 native bridge 定向测试通过；迁移契约 `--diff` 与 43 项扫描器自测通过。契约清单仍为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、360 审阅范围。完整门禁日志见 [`iteration-398-migration-gates.log`](../../build/optimization/iteration-398-migration-gates.log)，SHA-256 `e28a6936e6659cc78a4a48eef69e7d421ad03dd3646e03fcd51ce13142a685be`。本轮没有设备、运营商或真实通话验收。

P5 增加 0.025，累计 0.200 / 8.0；全项目累计 17.145 / 47.5 = 36.0947%（显示 36.1%，两位小数 36.09%，估算误差约 ±3 个百分点）。

本轮源码已通过离线门禁，但尚未打入 DMG；当前 [3.01 / build 301](../../dist/Switch-3.01-macOS27.dmg) 对应 Iteration 397。没有生成新 DMG，符合持续迁移阶段的发行约定。
