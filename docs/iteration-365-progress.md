# Iteration 365：IMS REGISTER qop 与 cnonce 迁入 Rust

日期：2026-10-11

本轮将 IMS REGISTER AKAv1-MD5 的 qop 选择和 cnonce 生成从 Go 移交给 Rust daemon。Rust 按既有兼容语义优先选择 `auth`，仅提供 `auth-int` 时选择 `auth-int`；没有受支持 qop 时沿用无 qop 模式。Rust 使用系统熵源生成 cnonce，并负责生成摘要和完整 Authorization 凭据头。Go 保留实体卡 AKA 交换、网络 stream/socket、SIP 分发和会话生命周期，并校验 Rust 返回的 qop、cnonce、计数与 AUTS。

验证：Rust IMS digest 专项测试通过；Go `simauth`、`voiceclient`、`imscore` 与 macOS bridge 测试通过；完整 Rust/Go/Swift/parity 迁移门禁通过，最终结果为 `migration gates passed`，见 [门禁日志](../build/optimization/iteration-365-migration-gates.log)。迁移契约清单 diff 通过（343 method/path：Rust 283、Go 60；127 个 Swift 调用点；353 个审阅范围）。

本轮 P3 增计 0.025 计划人周等值；累计为 1.865 / 8.0。全项目统一加权估值为 **16.345 / 47.5 = 34.4105%**，显示 **34.4%**（两位小数 **34.41%**）。估值误差约 ±3 个百分点。Iteration 209 的 31.4% 是历史快照，不再代表当前进度。

限制：本轮没有连接实体 SIM/读卡器，没有验证运营商 VoWiFi/SMS/通话，也未生成 DMG。IMS/VoWiFi 的生产会话生命周期和真实网络验收仍未完成；此切片不代表 IMS 已全 Rust 化。
