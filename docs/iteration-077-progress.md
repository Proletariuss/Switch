# Iteration 077：Rust 接管模块 AT 短信提交事务

日期：2026-10-05。改动只在 `/Users/proletarius/Desktop/Switch/src`，未访问原始参考目录。

## 完成内容

`switchd-rs` modem actor 现在接收严格的内部 `submit-sms` 请求，由 Rust 使用现有 SMS-SUBMIT codec 生成 modem 策略 PDU（随机拼接引用、无状态报告），然后独占完成 `AT+CMGF=0 → AT+CMGS prompt → PDU/CTRL-Z → +CMGS/OK`。模块多段短信在所有分段和 500 ms 模组间隔内保持 actor 事务互斥；中途有其他 AT 请求时不会穿插。每个分段都要求可解析的 `+CMGS` reference 和最终 `OK` 才记为确认；SIM 代次改变会阻止尚未提交的后续分段。

生产 Go modem manager 通过 Rust 串口 adapter 的可选 `SerialSMSSubmitter` 接口调用这个原子事务。未配置该接口的测试/旧兼容串口仍保留原 Go fallback。当前迁移边界仍不包括 Go 的公开发送路由选择、持久发送意图 claim/finalize、最终短信归档、USB 直连 AT fallback，以及 IMS、读卡器、N待等发送路径；这些仍阻塞移除 Go。

Rust 对确认不足统一返回 `complete=false` 和已确认分段数。Go 适配层将其作为提交错误传回，已有耐久发送意图保持 `unknown` 且禁止重放；只收到 `OK`、没有 `+CMGS` 也按未知结果处理。此轮未发送真实短信。

## 验证

- 伪终端集成覆盖：PDU 编码与确切 SMSC/TPDU payload、PDU 模式、提示符、`+CMGS`/`OK` 确认、多段间隔期间其他 AT 排队、过时代次零 I/O、PDU 模式拒绝不触发 CMGS、无 `+CMGS` 时保持未确认。
- `scripts/test-migration-gates.sh` 通过：`switchd-rs` 全套 **348 项**，Go vet 与全包测试，2,722 条 carrier replay，PCMA、IMS SDP 和 AMR parity。完整日志：`build/optimization/2026-10-05-iteration-077-migration-gates.log`。
- 定向 `cargo +1.98 fmt --all -- --check`、modem actor 33 项、Go `internal/modem`、Go Rust modem UDS adapter 测试通过。
- API/内部 UDS 清单审阅通过：**195 项**（Rust owner 92、Go owner 103）、127 个 Swift HTTP 调用点、69 个复核范围；不把路由归属作为端到端完成率。
- 本轮无 QMI/读卡器枚举、无实体卡读取、无真实短信或写卡，也未生成 DMG。测试仅使用合成号码与 PTY 假串口。

## 进度与未完成项

只将模块 AT 提交事务的 Rust 生产 owner 接线及离线故障覆盖计入 P2B：**26% → 28%**，增加 **0.16 计划人周**。整体从 **20.7% 调整至 21.1%**，即 **10.0 / 47.5 计划人周**，估算误差仍约 ±3 个百分点。仍然是 Rust+Go 混合运行架构。

本轮没有验证真实模块固件差异、自然短信失败/网络结果、daemon 强杀时的设备侧提交状态、Go public intent/final archive 退出、USB 直连 fallback、IMS/读卡器/N待短信以及无 Go 构建。错误结果继续由耐久 ledger 标为 unknown，不能视为消息未发送。
