# Iteration 383：REGISTER 已授权 Contact 绑定与租期裁决迁入 Rust

日期：2026-10-12。

生产 reader IMS REGISTER 成功响应现在通过既有 authenticated IMS codec UDS 调用 Rust。Rust 根据 Go SIP parser 提取的结构化事实执行 Contact 绑定选择（实例优先、URI 消歧、reg-id/instance 冲突过滤），并裁定 Contact expires、全局 Expires、请求租期回退、重复全局值一致性及零秒撤销。Go 仍负责 SIP 原始头语法解析与 URI 等价事实提取、socket/SIP事务、AKA、IPsec 和会话生命周期。

生产会话配置 Rust evaluator；Rust UDS 缺失、请求/返回值超界或返回错误时，REGISTER 成功流程失败关闭，不能回退 Go 的租期决策。未配置回调的独立兼容/测试路径保留既有 Go helper。错误只暴露稳定的租期错误码，不含 Contact 或用户身份。此切片迁移绑定选择与租期策略，不宣称 SIP parser 或 IMS runtime 全部迁出 Go。

验证：Rust IMS 策略测试 **6 passed**；daemon codec bridge **1 passed**；Go imscore/macOS 相关 lease、finalize 与 UDS bridge 测试通过。完整迁移门禁通过：[iteration-383-migration-gates.log](../build/optimization/iteration-383-migration-gates.log)：Rust workspace **1,957 passed / 6 ignored**（daemon **1,389 passed / 3 ignored**），Go 全包 test/vet、Swift BLE proof/manager regressions 与现有 carrier/USB/radio/HTTP/codec/SDP/AMR/MESSAGE parity 通过。契约审计 **343 method/path（Rust 283 / Go 60）**、127 Swift HTTP 调用点、358 审阅范围；`--write-inventory`/`--diff`、扫描器 **43 tests**、Rustfmt 与 `git diff --check` 通过。门禁日志 SHA256：`07412aa16d7f3fdfa8d5f4626f3d6aa52373056cfad82efea6b4eef21e8dd6ab`。

P3 +0.025，累计 **2.290 / 8.0 = 28.625%**；项目累计 **16.770 / 47.5 = 35.3053%**（显示 **35.3%**，两位 **35.31%**，估算误差约 ±3 个百分点）。未做实卡、运营商或长期会话验收；未生成 DMG。
