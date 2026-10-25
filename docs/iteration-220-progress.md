# Iteration 220：工作区迁移与原生 OPTIONS 应答

日期：2026-10-09。

用户明确授权在 `/Users/proletarius/Desktop/Switch_MAC` 继续。可编辑源码为该目录下 `src`，构建缓存、日志与运行状态保留在 `build`；AGENTS.md 已更新当前路径约束。历史日志中的旧路径保留作为当时证据，不批量改写。USB 状态、模块投影的兼容测试和多待集成脚本中硬编码的工作区约束也迁移到新目录；首次完整门禁因此中断，修正后重跑。

Rust IMS actor 在收到 OPTIONS 时沿来包连接返回 200 OK，然后仍将原事件交给消费方。事件携带 Unanswered/Sent/WriteUnknown 状态；写入失败不自动重试，也不伪造已成功。MESSAGE、NOTIFY、ACK 的业务处理不由此应答策略接管。

新增定向测试覆盖成功和写入不确定、连接关联、事件保留、一次应答及 KeepAlive 不触发应答。完整门禁日志：`../build/optimization/iteration-220-migration-gates.log`，重新执行通过：Rust workspace 格式及测试、Go 全仓 test/vet、既有协议兼容性门禁均通过，日志最终标记 `migration gates passed`。

生产 native 能力门禁仍保持关闭。整体迁移估计维持 31.4%，后续继续原生短信报告回执和完整事件消费，完成生产接管后再核算。没有实卡写入、实际短信或 DMG。
