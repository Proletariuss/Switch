# Iteration 369：IMS Security-Server 协商迁入 Rust

日期：2026-10-11

读卡器 VoWiFi 主注册路径现在把 `Security-Server` 的 ipsec-3gpp offer 解析、按客户端机制与 strict 策略选择最高 q 值 offer，以及原始 `Security-Verify` 字节序回显交给 Rust IMS codec owner。Go 收到类型化协商结果后仍负责构建 IPsec 策略、安装用户态 ESP transform、持有 SIP stream/socket、执行物理 AKA 和 IMS 生命周期。调用复用已有受认证 IMS codec endpoint；没有增加 HTTP 路由或扩大真实卡操作范围。未配置 Rust callback 的旧兼容入口保留 Go 解析逻辑，生产 reader session 已配置 callback，Rust 不可用或结果越界时不回退至 Go。

离线覆盖 Rust 协商成功、严格机制不匹配、超长输入、精确保留 Security-Verify 回显及参数映射；Go 覆盖 UDS bridge 的请求/响应边界和 IMS 生产配置消费。完整 `scripts/test-migration-gates.sh` 通过，包含 Rust workspace、Go 全包 test/vet、Swift BLE 与既有协议 parity。Rust migration contract `--write-inventory`/`--diff`、43 项 scanner tests、`cargo fmt --check` 和 `git diff --check` 通过。门禁日志：[iteration-369-migration-gates.log](../build/optimization/iteration-369-migration-gates.log)。没有实机、运营商、SIM、短信、通话或 DMG 验收。

P3 +0.025 计划人周等值，累计 **1.965 / 8.0**；项目累计 **16.445 / 47.5 = 34.6211%**（显示 **34.6%**，两位小数 **34.62%**，估值误差约 ±3 个百分点）。Go 仍承担 REGISTER/SIP 事务及会话生命周期、物理 AKA 与 IKE/ESP 安装；完整 IMS/VoWiFi native session owner 尚未验收。
