# Iteration 194：接听与挂断状态准入迁入 Rust

日期：2026-10-09。监督模式下公开 `POST /api/call/answer` 与 `POST /api/call/hangup` 的状态策略现由 Rust 负责。Rust 从认证的 Go call-state adapter 读取一次实时状态：接听只允许 `incoming`，其他状态以 `no_incoming_call` 拒绝；idle 挂断保留原兼容成功响应，不触发硬件动作。非 idle 挂断转交固定的内部动作。

Go adapter 保留动作前最后一次实时状态复核、answer 的 call-mode/RF readiness、SIM/USB transaction 准入、Rust modem actor 固定 AT 命令、通话事实/历史副作用，以及挂断后的音频路由清理。观察与动作之间仍有非原子窗口；SIP、IMS、媒体与实体 modem 生命周期不在本切片内，显式 Go 兼容启动仍保留旧公开处理器。Rust fake UDS 测试覆盖有效/无效接听状态、idle 挂断兼容语义和活动挂断的固定 adapter 转发；Go 测试覆盖内部 token 与严格空请求体。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace daemon **1,075 passed / 3 ignored**，Go 全仓测试与 `vet`、2,722 项载波回放、USB 状态/radio/HTTP method+HEAD/SDP/AMR parity 均通过。迁移契约工具 **43 项**自测、`--diff`、清单重建与 `git diff --check`通过。清单 **325** 条（Rust 260、Go 65，含 46 个 HEAD alias）、127 个 Swift HTTP 调用点、296 个审阅范围。`--require-rust-only` 仍按预期失败：默认 Go fallback 存在，**170** 条路由仍有 Go owner/dependency。未连接读卡器/SIM、未拨号或操作运营商网络，未生成 DMG。

保守计入 P2B **+0.025 计划人周等值**；累计 **14.620/47.5≈30.8%**（估算误差约 ±3 个百分点）。P2A **4.035/5.5（73.4%）**，P2B **5.325/8.0（66.6%）**，P3 **1.640/8.0（20.5%）**，P4 **0.750/4.5（16.7%）**。公开通话状态读取、拨号/接听/挂断命令执行、SIM/USB 与射频边界、通话历史、SIP/媒体及 Go 运行时退出仍待迁移。
