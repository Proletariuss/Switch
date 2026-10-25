# Iteration 372：首次 REGISTER 拒绝 fallback 策略迁入 Rust

读卡器 VoWiFi 的首次 REGISTER 被拒绝后，Go 原先根据配置/环境开关生成认证方式、PANI 与 Cellular 头字段变体，并根据状态码决定是否尝试下一变体。现在这些 carrier 策略由 Rust `proletarius-ims` 实现，并通过既有认证 IMS codec endpoint 提供。生产 reader session 传递活动 context；Rust 不可用、响应错误或结果越界时，REGISTER fallback 停止，不静默回退到 Go 策略。

兼容边界保留：配置启用与 `PROLETARIUS_IMS_INITIAL_REJECT_FALLBACK=1` 环境覆盖仍可启用回退；变体顺序保持原样；状态列表为 null 时默认仅对 403 重试，显式空列表保持禁用；apple_orange_uk 的 Cellular-Network-Info 排除和模板 PANI 默认值保持不变。未配置 Rust owner 的孤立兼容调用与旧单测仍保留 Go helper，生产 reader 路径明确配置 Rust owner。

验证：Rust 策略单测 3 项、daemon codec operation 2 项通过；Go IMS/macOS 定向测试通过。完整 `scripts/test-migration-gates.sh` 通过，日志：[iteration-372-migration-gates.log](../build/optimization/iteration-372-migration-gates.log)，包括 Rust workspace **1,379 passed / 3 ignored**、Go 全包 test/vet、Swift BLE 证明与 38 组 manager 序列、carrier 2,722 cases、USB/radio/HTTP/HEAD/codec/SDP/AMR/MESSAGE parity。迁移契约扫描 `--diff` 通过：343 method/path（Rust 283 / Go 60）、127 Swift 调用点、356 审阅范围；`cargo fmt --check` 与 `git diff --check` 通过。

本轮不新增 HTTP 路径，不迁移 SIP socket/事务等待、物理 AKA、IPsec 安装或 IMS 生命周期。未操作实体卡、未做运营商/短信/通话测试、未生成 DMG。

P3 +0.025，累计 **2.040 / 8.0 = 25.5%**；整体 **16.520 / 47.5 = 34.7789%**（显示34.8%，两位34.78%，估算误差约±3个百分点）。Go session/compatibility 政策边界仍需后续收敛，不能视为 Go owner 已退出。
