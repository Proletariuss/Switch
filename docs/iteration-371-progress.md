# Iteration 371：REGISTER Contact 头策略接入 Rust

读卡器 VoWiFi 主注册和受保护重注册现在通过现有 IMS codec endpoint，由 Rust 构造 `Contact` header。Rust formatter 承担 IPv4/IPv6（含 IPv4-mapped）地址与端口格式、android_default/GSMA/legacy 模式、载波参数顺序、`icsi_ref` 编码、SMS-only 能力裁剪及 expiry 规则；Go 只提供当前 reader/template 事实并把 Rust 结果放入现有 SIP 请求。生产 callback 传递真实会话 context，Rust 不可用、取消或响应无效时 REGISTER 构造失败关闭。未切换的呼叫/USSD Contact 路径仍走现有 Go helper。

定向覆盖：Rust formatter 5 项（默认头、GSMA/legacy、SMS-only、IPv4-mapped、非法地址/注入），Rust daemon codec operation，Go IMS bridge 与 production builder/cancel 测试通过。完整 `scripts/test-migration-gates.sh` 通过，日志：[iteration-371-migration-gates.log](../build/optimization/iteration-371-migration-gates.log)。Rust workspace **1,923 passed / 6 ignored**（daemon **1,377 passed / 3 ignored**），Go 全包 test/vet、Swift BLE proof/manager regressions、38 组 BLE manager sequence、carrier **2,722** cases、USB/radio/HTTP/HEAD/codec/MESSAGE parity、contract inventory/diff、`cargo fmt --all --check` 和 `git diff --check` 均通过。

本轮没有实机或运营商 REGISTER、读卡、短信、通话验收；没有生成 DMG。此切片只接管 Contact 格式化与该路径的 SMS-only 参数策略，不代表 SIP parser/socket、物理 AKA、IPsec transform、事务等待或 IMS 生命周期已迁移。

P3 +0.025，累计 **2.015 / 8.0 = 25.1875%**；全项目累计 **16.495 / 47.5 = 34.7263%**（显示 **34.7%**，两位小数 **34.73%**，估算误差约 ±3 个百分点）。Go 仍承担 IMS 在线会话及呼叫/USSD Contact 兼容路径。详见 [Iteration 370](iteration-370-progress.md) 和[进度复核账本](optimization/all-rust/progress-reassessment-2026-10-10.md)。
