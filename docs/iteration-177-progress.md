# Iteration 177：PLMN 显示名投影迁入 Rust

日期：2026-10-09。所有源码和验证产物均位于 `/Users/proletarius/Desktop/Switch/`。

本轮把 PLMN→运营商显示名映射接入 Rust 共用模块，并用于受监督串口 typed operator query 和 USB AT 状态投影。Go 串口适配器直接消费 Rust 返回的显示值；USB 状态适配器要求私有 DTO 中的 `operator_name` 字段，并在蜂窝信息受抑制时同时校验原始 identifier 与显示名为空。未知 PLMN 仍返回规范化原值，空值保持为空。

USB 投影仅从与旧 Go 正则相同的引号 COPS 字段派生显示名。旧 `CHN-UNICOM` 特殊回退只保留在 `operator_identifier`，不会被错误显示为运营商名；飞行模式会清除两者。USB 私有 Rust-Go DTO 从 15 个字段增至 16 个，公开 `/api/status` JSON 保持不变。Go 映射仍服务尚未迁移的 QMI/MBIM 与显式独立兼容路径，因此这只完成监督串口/USB 投影的一段迁移。

验证结果：

- Rust PLMN 映射、USB 状态投影和 modem actor typed query 测试通过。
- Go USB 状态及 typed operator 边界测试通过；冻结 Go 夹具与 Rust USB 投影逐字段差分通过，覆盖原值、未知值、回退和飞行模式抑制行为。
- `cargo fmt --check`、相关 Go `gofmt` 检查、`git diff --check` 及迁移契约 `--diff` 通过。契约保持 307 条 method/path（Rust 240、Go 67，含 40 个 HEAD alias）、127 个 Swift HTTP 调用点和 286 个审阅范围。
- `scripts/test-migration-gates.sh` 通过：Rust daemon **992 passed/3 ignored**；Go 全仓测试和 `vet` 通过；2,722 项载波回放、USB/radio/HTTP method+HEAD/codec+SDP parity 通过。

保守计入 P2B **+0.025 计划人周等值**，由 4.975 增至 **5.000/8.0（62.5%）**；总体由 14.095 增至 **14.120/47.5≈29.7%**（估算误差约 ±3 个百分点）。P2A 为 3.935/5.5（71.5%）。未操作读卡器或号卡，未开展运营商实测、Go-free 构建/运行验收或 App 打包；未生成 DMG。全 Rust 目标仍未完成。
