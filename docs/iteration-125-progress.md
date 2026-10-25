# Iteration 125 — 通话模式 USBCFG 写入迁入 Rust

日期：2026-10-08。

Call-mode启用与备份还原中的Go直写USBCFG已改成Rust module-radio typed compare-and-write。Rust仅在CallMode reservation内执行：确认CLCC状态允许、读取并精确匹配调用方预期的USBCFG快照、写入单一目标值、回读并验证VID/PID与未修改位不变，修改位只能处于原值或目标值。若预读已是目标配置则不重复写。明确模块拒绝不会重试；写入回执丢失、owner撤销或回读异常均维持不确定结果，reservation禁止后续业务操作，要求先释放并刷新核实。

Go仍生成启用目标并绑定模块身份；保留状态/备份业务流程、ADB授权与helper、重启和恢复编排。现无Go生产路径直接查询、解析或写入USBCFG。

离线验证：Rust workspace **759通过、0失败、3忽略**；Go `cmd/proletarius-macos` race测试、`go vet`、Rustfmt、`git diff --check`、42项迁移契约自测及普通264项契约扫描通过。契约计数未变（Rust184/Go80、127个Swift调用、210个审阅范围）。测试覆盖预期快照变化不写入、一次写入与回读、拒绝不重放、回读不确定标记及typed reservation授权。没有操作实体模块、读卡器或号卡；没有生成DMG。

本轮计入P2B **+0.025计划人周**，由3.900升至3.925/8.0（49.06%）；整体由12.370升至**12.395/47.5≈26.1%**（估算误差约±3个百分点）。Go call-mode业务状态机、ADB/helper、重启恢复和设备验收仍待迁移/核验。
