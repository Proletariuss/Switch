# Iteration 197：本地短信清空策略迁入 Rust

日期：2026-10-09。监督模式下 `POST /api/sms/clear` 现在由 Rust 拥有公开路由和本地清空决策。Rust 冻结用户看到的短信列表及 Go 易变分区，按快照中的稳定 ID 删除本地归档，并通过认证的内部桥同步 Go 缓存与 USB 预览。Go 缓存只删除仍与冻结记录完全匹配的项目；清空期间被修改或新到达的记录会保留。空缓存明确编码为 `[]`，不会再以 `null` 触发归档快照拒绝。

物理 SIM/ME 清空及混合存储清空仍交由原 Go 安全事务执行，保留既有租约、读回和确认语义；因此本轮是公开入口和纯本地清空策略迁移，不是物理短信存储或完整短信生命周期的 Go-free 验收。跨进程失败仍返回可诊断阶段码，清空后刷新用于核对持久与易变状态。

验证通过：Rust 定向用例 **4/4**、Go 清空兼容适配器用例通过；独立 Go/Rust 进程集成 **31/31**，含本地清空和空快照同步；完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,086 passed / 3 ignored**，Go 全仓 `go test` 与 `go vet`、2,722 项载波回放以及 USB/radio/HTTP method+HEAD/SDP/AMR parity 通过。契约扫描器43项自测、清单 `--diff`、Rust格式检查及 `git diff --check` 通过。当前清单330条（Rust265、Go65，含48个HEAD alias）、127个Swift调用点、298个审阅范围。Rust-only门禁仍按预期失败：默认Go fallback存在，175条路由仍有Go所有权或运行依赖。集成报告：`/Users/proletarius/Desktop/Switch/build/reports/multistandby-iteration197-rerun.log`；完整门禁日志：`build/reports/migration-gates-iteration197.log`。未访问实体读卡器/SIM或运营商网络，未构建App/DMG。

保守计入P2A **+0.025计划人周等值**；累计 **14.695/47.5≈30.9%**。P2A **4.060/5.5（73.8%）**，P2B **5.375/8.0（67.2%）**，P3 **1.640/8.0（20.5%）**，P4 **0.750/4.5（16.7%）**。Go 仍承担物理短信存储事务、实时设备与会话事实、VoWiFi/IMS、通话副作用及其他硬件生命周期。
