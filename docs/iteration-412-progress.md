# Iteration 412：外呼 SIP 拒绝原因分类迁入 Rust

外呼失败时，SIP 状态码和 Reason/Q.850 原因到用户提示的分类现由 Rust 执行。Go 不再保留重复的 487/Q.850 映射逻辑；生产 reader call runtime 通过现有认证 IMS codec endpoint 请求 Rust 结果，校验后用于最终错误展示。Reason header 优先级、487 通用文案、cause 1/20/127 提示和非 487 文案与既有行为一致。Go 仍负责收取响应、ACK、呼叫状态和资源清理。

Rust policy/daemon 单测、Go adapter/runtime 测试和 Rust fixture 对照通过；完整门禁通过：Rust workspace 1,421 passed / 3 ignored，Go vet/全包、Swift BLE regressions、既有协议 parity 与新增拒绝文案 parity 均通过。契约 diff 与 44 项扫描器自测通过：346 method/path（Rust 286 / Go 60）、127 Swift HTTP 调用点、370 审阅范围。日志：[iteration-412-migration-gates.log](../build/optimization/iteration-412-migration-gates.log)，SHA-256 `e6949635b87100c3e731115bee4f33448c3535ca0a0e88c0cdcded16e6ea7a74`。

P3 +0.025，累计 **2.790 / 8.0（34.88%）**；全项目 **17.495 / 47.5 = 36.83%**。未运行运营商实机呼叫；全 Rust 退出门禁仍在7个 Go 构建/运行/源码类别受阻。本轮没有生成 DMG；上轮 3.08/build308 iteration411 包未包含本轮。详见[进度复核账本](optimization/all-rust/progress-reassessment-2026-10-10.md)。
