# Iteration 195：公开通话状态投影迁入 Rust

日期：2026-10-09。监督模式下 `GET /api/call/status` 由 Rust 作为公开响应入口。Rust发起带内部凭证的 `observe:true` 调用，并透传 Go 状态适配器的兼容 JSON/status；适配器不可用时返回UI兼容的 `200 {"state":"unknown","incoming":false,"active":false}`。公开路由的GET/HEAD方法准入现由Rust声明，HEAD保留GET表示头并去除响应体。

Go继续拥有实时模块CLCC/URC观测以及既有通话历史和radio观察副作用；显式Go兼容启动仍保留原handler。此处只完成公共HTTP投影，硬件事实来源、SIP/IMS传输、通话会话生命周期与动作边界未迁移。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,078 passed / 3 ignored**，Go全仓`go vet`与`go test`通过，2,722项载波模板回放、USB状态、模块radio、HTTP method+HEAD、SDP与AMR parity通过。迁移契约扫描通过，清单重建通过，`git diff --check`通过。源码契约326条（Rust262、Go64，含47个HEAD alias），127个Swift HTTP调用点、296个审阅范围。`--require-rust-only`按预期失败：默认Go fallback仍存在，171条路由仍有Go所有权或运行依赖。未访问实体读卡器/SIM、未拨号或操作运营商网络，未生成App或DMG。

保守计入P2B **+0.025计划人周等值**；累计 **14.645/47.5≈30.8%**（估算误差约±3个百分点）。P2A **4.035/5.5（73.4%）**，P2B **5.350/8.0（66.9%）**，P3 **1.640/8.0（20.5%）**，P4 **0.750/4.5（16.7%）**。
