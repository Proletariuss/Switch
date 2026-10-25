# Iteration 170：N 待启动恢复轮询归 Rust

日期：2026-10-08。源码、测试与构建材料均在 `/Users/proletarius/Desktop/Switch/`。

Rust daemon 现在持有进程级 N 待恢复任务，每 5 秒检查一次持久 `keep_online` 意图与 Go 后端就绪代次。发现待恢复意图后，Rust 通过已有认证的 `runtime-snapshot` 适配器请求读卡器盘点；读卡器未就绪、盘点失败或响应不完整时继续重试。每个 Go 后端代次成功协调一次后停止重复探测；Go 子进程重启或 Rust supervisor 重启后会重新协调。没有持久在线意图时不触发读卡器探测。

移除了 Go `startMultiStandbyRecovery` 五秒 ticker。Go 的 `runtime-snapshot` 现在明确拒绝在没有选中读卡器时执行刷新，避免恢复探测碰触未初始化设备。**本轮只迁移启动恢复轮询与代次去重**；Go 仍负责真实硬件盘点、N 待物理 Profile 会话启动、broker 与每 Profile 网络恢复 timer，并保留既有的策略 IPC 故障兼容路径。该结果不代表 N 待 session/broker 或物理恢复已完成 Rust 化。

验证通过：Rust 定向恢复状态测试 **2 项**；Go runtime snapshot/recovery 定向测试；Rust supervisor + Go fixture **26 项跨进程检查**，覆盖无意图不探测、不可用时重试、同一 Go 代次成功后去重、Go 进程重启及完整 Rust 重启后的恢复。`scripts/test-migration-gates.sh` 全量通过：Rust workspace **1,463 项通过、6 项忽略**，其中 daemon **966 项通过、3 项忽略**；Go 全仓 `test/vet`、2,722 项载波回放、USB/radio/codec/HTTP-HEAD parity 全部通过。契约工具 **43 项**自测通过，契约 `--diff` 通过，清单维持 **296 条**（Rust 232、Go 64，含 38 个 HEAD alias）、127 个 Swift HTTP 调用点、277 个审阅范围；`git diff --check` 通过。

Rust-only 退出门禁仍因默认 Go fallback 和 150 条 Go owner/dependency 路由未通过。未连接实体读卡器或 SIM，未做运营商/长稳验收，未构建 App，也未生成 DMG。

保守计入 P4 **+0.025 计划人周等值**，从 0.575 增至 **0.600/4.5（13.33%）**；总体由 13.845 增至 **13.870/47.5（29.2%）**，估算误差约 ±3 个百分点。仍待迁移：N 待卡片仲裁、操作执行与完整会话生命周期、每 Profile timer、Go 兼容路径退出，以及真实设备和长稳验证。
