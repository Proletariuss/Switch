# Iteration 122 — IMS/VoLTE 配置事务迁入 Rust

日期：2026-10-08。

Rust `module-radio` 新增固定 `call_mode_voice` action，仅接受 CallMode reservation 和 typed target。事务执行 CLCC 安全门禁、读取 IMS 与 `volte_disable`、按 `volte_disabled` 后 IMS 的顺序条件写入，并在写入后查询确认。由于配置要重启后生效，每个字段的回读只接受原值或目标值；明确终端拒绝停止，写入响应不完整则标记不确定、关闭该传输，不重放。Go `applyCallModeVoiceConfiguration` 只调用Rust并验证 typed结果；高层call-mode状态机、备份、安全身份核验、USB/ADB/helper、重启与恢复仍由Go持有。

验证：对应全量 Rust daemon **750通过、3忽略**；Go `cmd/proletarius-macos` 全包 race、`go vet`、契约工具42项自测、Rustfmt和普通路由扫描通过。没有实体设备测试。

P2B增加 **0.025计划人周**，由3.825到3.850/8.0（48.13%）；总估值从12.295到12.320/47.5≈25.9%。Go call-mode状态/硬件生命周期和最终Go-free验收未完成。
