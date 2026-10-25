# Iteration 123 — IMS/VoLTE 只读状态查询迁入 Rust

日期：2026-10-08。

删除Go侧IMS/VoLTE QCFG response parser及直接 `runATCommand` 查询。Rust新增只读 `call_mode_voice_read` typed action，在一项Rust reservation中按序执行 `AT+QCFG="ims"` 和 `AT+QCFG="volte_disable"` 并返回已验证元组，不包含任何写入。该动作只允许已有 `CallMode` 或 `Call` lease；Go没有可复用lease时申请短CallMode lease，等待查询完成后释放。模块radio action逐步使用具体reservation/token/sequence和operation ID，Go端拒绝没有已验证Voice tuple的响应。Call-mode高层状态判断、USB配置、ADB/helper、重启及恢复流程仍在Go。

验证：Rust daemon **752通过、0失败、3忽略**；Go主包race与`go vet`通过；Rustfmt、`git diff --check`通过；迁移契约工具42项自测与普通扫描通过，契约为264（Rust184/Go80，含30个HEAD别名），210个审阅范围、127个Swift HTTP调用点。Release `scripts/build.sh` 构建 `/Users/proletarius/Desktop/Switch/dist/Switch.app` 2.86/build286 arm64，`codesign --verify --deep --strict`通过。应用仍嵌入Go `switchd` 和Rust `switchd-rs`，说明最终零Go构建/运行/嵌入门禁仍未满足。未操作读卡器/实体卡、未进行真实呼叫或生成DMG。

P2B计 **+0.025计划人周**至3.875/8.0（48.44%）；总体为12.345/47.5≈26.0%（±3个百分点）。
