# Iteration 189：DTMF 公开准入策略迁入 Rust

日期：2026-10-09。范围仅限 `/api/call/dtmf` 的输入校验、活动呼叫准入和向已有 Go 命令适配器的派发决策。

Rust 拒绝非单字符 ASCII DTMF 和非活动呼叫，不触碰Go adapter；有效符号才被提交至原有适配器。Go 仍持有实时呼叫状态观察、SIM/USB互斥及模块AT事务。

`call_control` 单测覆盖有效/无效字符、呼叫状态门禁及适配器错误映射。完整迁移门禁见 [Iteration 192](iteration-192-progress.md)。保守计入P2B +0.025计划人周；本迭代不单独代表完整呼叫控制迁移。
