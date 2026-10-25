# Iteration 188：模块语音 AT 命令由 Rust modem actor 构造并执行

日期：2026-10-09。源码与验证均在 `/Users/proletarius/Desktop/Switch/` 内。没有操作实体读卡器、SIM 或运营商网络，也没有生成 DMG。

模块拨号、接听、挂断和 DTMF 的固定 AT 命令现由 Rust modem actor 根据严格的 `action/value` 请求构造并通过独占串口执行：`dial` 生成 `ATD<number>;`，`answer` 生成 `ATA`，`hangup` 生成 `ATH`，DTMF 仅接受单个 `0-9/*/#` 并生成 `AT+VTS=`。号码和动作在 Rust 端再次校验，未知动作、任意 AT 文本和无效参数在触碰串口前拒绝；每个动作使用固定 10 秒或 5 秒命令期限，并沿用 actor 的队列、结果和 watchdog 语义。

Go 继续提供现有公开呼叫路由、SIM 互斥、USB transaction 准入、call state/URC 观察、历史副作用、SIP 与音频生命周期。显式兼容串口仍保留原固定命令路径。此次只迁移模块 AT 命令生成及串口执行，不代表公开拨号接口、呼叫状态机或通话业务已完全脱离 Go。

验证通过：完整 `RUST_TEST_THREADS=1 scripts/test-migration-gates.sh`。Rust workspace **1,045 passed / 3 ignored**；Go 全仓测试与 `vet`、2,722 项载波回放以及 USB 状态、radio、HTTP method+HEAD、codec-SDP parity 均通过。Rustfmt、迁移契约 43 项自测、契约 `--diff` 与 `git diff --check` 通过。源码契约清单为 **320 条**（Rust 256、Go 64，含 46 个 HEAD alias）、127 个 Swift HTTP 调用点、294 个审阅范围。

按计划人周保守计入 P2B **+0.025**，从 **5.225/8.0** 提至 **5.250/8.0（65.6%）**；累计 **14.470/47.5≈30.5%**（误差约 ±3 个百分点）。P2A 为 3.985/5.5（72.5%），P3 为 1.615/8.0（20.2%），P4 为 0.750/4.5（16.7%）。Go-only 呼叫控制状态机、SIM/USB admission、SIP/IMS 与语音生命周期、硬件会话 owner、Go-free 构建/运行及真实设备验收仍未完成。
