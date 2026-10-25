# Iteration 199：按发送方删除短信策略迁入 Rust

日期：2026-10-09。范围限定在 `/Users/proletarius/Desktop/Switch`。

监督模式 `POST /api/sms/delete-sender` 现在由 Rust 拥有公开路由、发送方规范化、冻结快照选择、本地归档删除与唯一短信计数。Rust 使用确认的稳定 ID 修剪快照；对于部分成功只应用已确认的结果，不重放物理删除。只有快照中存在 SIM/ME 实体记录或需要校验的隐藏证明行时，才调用认证 Go 物理适配器。

Go 适配器负责重新核对当前卡片身份、物理来源证明及删除凭据，并继续通过既有硬件安全事务操作 SIM/ME；Go 易变收件箱的精确修剪也仍在此适配器。Rust 负责删除归档和未被物理 owner 处理的 USB 预览记录。显式 Go 兼容启动保留旧公开 handler，因此本轮迁入 Rust 的是监督模式业务策略，不代表该功能已经无 Go 依赖。

验证结果：

- Rust 定向测试：**4 项通过**；Go 物理适配器认证、凭据与失效证明测试通过。
- 独立 Go/Rust 进程集成：**33 项通过**，含按发送方删除后的归档和缓存一致性验证；运行记录见 [进程日志](../../build/reports/multistandby-contract-process.log) 与 [机器可读结果](../../build/reports/multistandby-contract-results.json)。
- 完整迁移门禁：Rust workspace **1,094 passed / 3 ignored**；Go 全仓测试与 `go vet`、2,722 项载波回放、USB/radio/HTTP method+HEAD/SDP/AMR parity 通过。
- 迁移契约扫描器 **43 项自测通过**，源码 `--diff` 通过；清单 **332 条**（Rust 267、Go 65，含48个HEAD alias）、127个Swift调用点、298个审阅范围。
- `git diff --check` 通过。Rust-only退出检查仍失败：默认Go fallback存在，**177条**路由仍有Go owner或运行依赖。
- 未连接读卡器或实体SIM，未进行通话、短信或运营商网络测试；未构建App或DMG。

按原计划人周中值保守增加 P2A **0.025 人周等值**：累计 **14.745/47.5 = 31.0%**（估算误差约±3个百分点）。P2A 为 **4.110/5.5（74.7%）**，P2B 为 **5.375/8.0（67.2%）**，P3 为 **1.640/8.0（20.5%）**，P4 为 **0.750/4.5（16.7%）**。详细方案状态见[全 Rust 执行看板](optimization/2026-10-04-all-rust-execution.md)。
