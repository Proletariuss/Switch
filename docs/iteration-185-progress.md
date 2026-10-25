# Iteration 185：AT 控制入口与 eSIM 健康投影迁入 Rust

日期：2026-10-09。源码、构建与测试均在 `/Users/proletarius/Desktop/Switch/` 内；本轮未生成 DMG、未连接读卡器或访问实体 SIM。

公开 `POST /api/at` 现由 Rust 校验请求、执行 VoWiFi/射频冲突策略并调用有界的认证执行适配器。Go 适配器只保留现有模块 SIM/APDU 互斥锁并将命令交给 Rust module-radio 执行器；串口执行和 RF 策略由 Rust 持有。原 Go 公开处理器只保留显式兼容启动。

公开 `GET /api/esim/health` 也由 Rust 处理。Rust 先读取按 Go backend generation 绑定的 eUICC Profile 事实，自行选择启用 Profile；只有存在有效活动 Profile 时，才请求第二阶段模块状态事实，再由 Rust完成 ICCID 对齐、注册状态分类及兼容响应。Go 继续作为模块 eUICC/APDU 与 USB-AT 观察适配器，因此这不是实体 eSIM 生命周期或 Go-free 迁移。

验证通过：完整 `scripts/test-migration-gates.sh`（串行 Rust 测试线程）；Rust workspace **1,036 passed / 3 ignored**，Go 全仓测试与 `vet`、2,722 项载波回放，以及 USB/radio/HTTP method+HEAD/codec-SDP parity通过。迁移扫描器 **43 项自测**、契约扫描及 `git diff --check`通过。合同清单 **317 条**（Rust 253、Go 64，含45条 HEAD alias）、127 个 Swift HTTP调用点、292 个审阅范围。此清点不是迁移完成率。

保守计入：P2A **+0.025 计划人周等值**至 **3.985/5.5（72.5%）**；P2B **+0.025**至 **5.175/8.0（64.7%）**。整体 **14.395/47.5≈30.3%**（估算误差约±3个百分点）；P3 **1.615/8.0（20.2%）**，P4 **0.750/4.5（16.7%）**。Go-free 构建/运行、模块硬件与剩余设备业务、reader/profile session、IMS/SIP/VoWiFi 生命周期、语音以及实机/长稳验收仍未完成。详情和代码边界见[执行记录](optimization/2026-10-04-all-rust-execution.md)。
