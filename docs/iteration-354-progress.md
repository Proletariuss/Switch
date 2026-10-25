# Iteration 354：N待稳定在线时钟与观测缺口收归 Rust

日期：2026-10-11。工作目录：`/Users/proletarius/Desktop/Switch/src`。未操作实体号卡/短信，未生成 DMG。

## 实现

N待恢复策略此前由 Go 记录 `readySince`、`lastObservation`，计算稳定在线时长与观测缺口，再把计算值交给 Rust。现在 Go 仅提交受限的 Profile ID、原因和在线事实；Rust 自己给观察事件打时间戳，在耐久 runtime journal 中维护 `ready_since_ms`，并根据最近观察和当前时钟计算稳定在线时长、发现超过 60 秒的观测缺口，再执行既有恢复策略。Go 的本地稳定在线时钟、观测时间戳和对应 elapsed/gap DTO 均已移除。

旧 journal 中新增的 `ready_since_ms` 字段可选，旧版格式仍可读取；旧数据有累计稳定时长但缺少可证明的起点时，Rust 清除该不确定的稳定时长，不据此提前重置失败预算。Rust journal 写入失败仍会阻止自动恢复策略返回，Go 保持降级状态并等待明确的手动重试或停止。

Go 仍负责采集底层会话事实、启动/停止 N待 session、SIP/IMS 行为和物理卡流程；本轮没有迁移真实网络会话或宣称实机稳定性通过。

## 验证

- Rust runtime journal 定向测试 23 项通过，含 Rust 时钟推进、稳定在线窗口、观测缺口、旧格式缺少新字段时失败关闭、重启恢复及现有策略语义。
- Rust 私有 runtime observation 集成测试通过；Go `cmd/proletarius-macos` 全包测试通过。
- 完整迁移门禁通过：Rust workspace、Go 全包测试与 `go vet`、Swift BLE 事件序列、载波/USB/IMS/codec/HTTP parity 检查均通过。日志：[iteration-354-migration-gates.log](../build/optimization/iteration-354-migration-gates.log)。
- 契约扫描通过：343 method/path（Rust 283 / Go 60）、127 个 Swift HTTP 调用点、353 个审阅范围；扫描器 43 项自测通过。没有公开 method/path 变化。

## 当前进度

本轮 P4 计入 +0.025 计划人周等值，累计 **16.095 / 47.5 = 33.8842%（显示 33.9%，两位小数 33.88%）**。零 Go 构建/运行/嵌入依赖和全部实机/业务验收仍未完成。
