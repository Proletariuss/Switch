# Iteration 187：模块 AT+CLCC 查询迁入 Rust 串口 actor

日期：2026-10-09。源码和验证均在 `/Users/proletarius/Desktop/Switch/` 内；未访问实体读卡器或 SIM，未生成 DMG。

模块通话状态查询现在调用 Rust modem actor 的固定 typed query，由 Rust actor 发出 `AT+CLCC` 并在 Rust 投影 `idle/incoming/dialing/alerting/active` 状态。Go 不再为受监督 Rust 串口单独编排此命令或二次请求 Rust解析器。Go 继续保持模块 SIM 互斥与 USB transition 准入、10秒内新鲜来电 URC 兜底、观测代次校验和 Rust call-facts 历史归档；没有 typed-query 的显式兼容串口保留原命令+Rust parser 路径。公开 `/api/call/status`、拨号/接听/挂断、SIP/VoWiFi会话与媒体全链路仍未迁移。

验证通过：完整 `scripts/test-migration-gates.sh`（Rust 测试串行）；Rust workspace **1,043 passed / 3 ignored**，Go 全仓测试与 `vet`、2,722 项载波回放，以及 USB 状态、radio、HTTP method+HEAD、codec-SDP parity 均通过。新增 Rust 伪串口测试验证 actor 实际发出固定 `AT+CLCC` 并准确投影 incoming 状态；Go Manager 类型校验、Rust modem adapter schema 和相关定向测试通过。迁移契约43项自测、契约 `--diff`、Rustfmt 与 `git diff --check`通过。契约清单 **319 条**（Rust 255、Go 64，含46个 HEAD alias）、127 个 Swift HTTP 调用点、294 个审阅范围。

保守计入 P2B **+0.025 计划人周等值**至 **5.225/8.0（65.3%）**；累计 **14.445/47.5≈30.4%**（估算误差约±3个百分点）。P2A 为3.985/5.5（72.5%），P3 为1.615/8.0（20.2%），P4 为0.750/4.5（16.7%）。SIM互斥、通话状态与历史时序、SIP/语音、设备/eSIM操作和Go-free交付仍需继续迁移与验收。详情见[全 Rust 执行看板](optimization/2026-10-04-all-rust-execution.md)。
