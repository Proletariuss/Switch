# Iteration 346：模块 SIM 短信中心路由策略迁入 Rust

模块 SIM 元数据与 `AT+CSCA?` 已由 Rust USB AT owner 在同一次有界身份事务中读取，但 Go 仍独立决定短信中心地址的规范化、卡内地址优先、AT 回退，以及缺少目标时生成 `tel:`。现在 Rust 在构造该代 SIM 身份快照时统一解析并输出有效 `smsc` / `smsc_target`；Go 直接消费结果，删除对应解析和优先级副本。运营商配置的 PSI 目标仍优先，非法地址不生成路由，也不从先前 Profile 复用状态。

离线测试覆盖卡内地址优先、AT 回退、目标保留/默认生成、格式字符规范化和无效值拒绝。Rust 专项 2 项、Go 模块 SIM 定向测试通过。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace 1344 passed / 3 ignored、Go `vet` 与全部测试包、Swift BLE 物理完成/manager 回归及 carrier、USB、radio、HTTP HEAD、codec、SDP、MESSAGE parity 均通过。Rust 有 2 个既有 dead-code warning，Go linker 报 duplicate `-lusb-1.0` warning；均不影响门禁通过。契约路由数量不变（343 method/path：Rust 282 / Go 61），127 Swift 调用点和 354 审阅范围不变。

计分：这是 Iteration 335 明确留在 Go 的 SMSC 投影规则，现生产调用已由 Rust 完整负责，按 P2B 增加 0.025 人周等值；总分 **15.895 / 47.5 = 33.5%**。没有实体读卡、短信发送、VoWiFi 启停或运营商验收；没有生成 DMG。9eSIM `6982` 仍按用户要求排除。
