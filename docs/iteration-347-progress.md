# Iteration 347：USB 状态中的号码回退移交 Rust

DJI USB AT 的 ICCID、CNUM 与设备状态字段已由 Rust owner 在同一固定 session 内采集，但 Go 状态适配器仍会拿 ICCID 额外请求 Rust profile-notes 服务，再用返回号码替换缺失的 CNUM。现在生产 Rust USB AT owner 注入同一 profile-notes store，并在状态快照生成后按该快照 ICCID 补号码；已有有效 CNUM 保持优先。Go 删除额外 RPC 和本地回填分支，继续负责现有模块状态缓存与生命周期发布。

定向测试验证缺失号码回填、已知 CNUM 不覆盖、无备注时回退为空；另有带假 USB AT transport 与临时 profile-notes 文件的 owner 集成测试，证明 Rust 按同一状态快照 ICCID 取回已保存号码。Go USB 状态适配测试改为消费 Rust 已完成的回退结果。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1346 passed / 3 ignored**，Go `vet` 与全包测试、Swift BLE 物理完成/manager 回归及 carrier、USB、radio、HTTP HEAD、codec、SDP、MESSAGE parity 均通过。API 契约为 **343 method/path（Rust 282 / Go 61）**、127 Swift 调用点、354 审阅范围；契约 `--write-inventory` 后 `--diff` 通过。完整日志：[iteration-347-migration-gates.log](../build/optimization/iteration-347-migration-gates.log)。

计分：这段由 Go 驱动的 profile-notes 查询与号码回退是独立生产决策，现已在 Rust USB AT 状态投影中闭合，P2B 增加 **0.025 人周等值**。总分 **15.920 / 47.5 = 33.5158%（显示 33.5%，精确到两位为 33.52%）**。本轮只做离线验证；无实体读卡、发信、运营商操作或 DMG。9eSIM `6982` 仍按用户要求排除。
