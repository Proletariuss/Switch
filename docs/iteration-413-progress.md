# Iteration 413：拨号目标策略完全交由 Rust

生产拨号现在在呼叫状态变更和媒体分配之前，先经现有认证 IMS codec 接口由 Rust 校验并规范化目标。Rust 同时负责空目标、紧急号码禁拨与当前不支持 USSD 语音拨号的短码判定；初始 INVITE 直接复用已校验的 URI。Go 中重复的号码规范化、紧急号码判断、USSD 短码判断以及缺少 callback 时的本地 fallback 已删除；Rust 不可用时失败关闭，呼叫状态和媒体资源不变。Go 继续承担 SIP/dialog、会话、媒体与 socket 生命周期，未扩大为全 Rust 呼叫接管。

验证通过：Rust workspace **1,421 passed / 3 ignored**；Go 全包测试与 `go vet`、Swift BLE 回归、2,722 项 carrier replay、USB/radio/codec/SDP/HTTP method+HEAD parity 均通过。定向 Go↔Rust 拨号结果和错误映射、缺少 Rust normalizer 时的状态不变测试通过。契约 `--diff` 通过：346 method/path（Rust 286 / Go 60，含 49 个 HEAD 别名）、127 Swift HTTP 调用点、370 个审阅范围；扫描器 44 项自测通过。全 Rust 退出门禁仍有 7 类 Go 构建/运行依赖。

完整门禁日志：[iteration-413-migration-gates.log](../build/optimization/iteration-413-migration-gates.log)，SHA-256 `5fd988feeaa0677a7c2ed83ec9a170d8c458533b5da0df0f89328f90c702fb4c`。未做运营商实机呼叫或实体卡操作；最近的 3.08/build308 Iteration 412 DMG 不包含本轮代码。

P3 +0.025，累计 **2.815 / 8.0（35.19%）**；全项目 **17.520 / 47.5 = 36.88%**（估算误差约 ±3 个百分点）。全 Rust 改造尚未完成。
