# Iteration 385：REGISTER E.164 回拨号码提取迁入 Rust

成功 IMS REGISTER 的 `P-Associated-URI` 身份解析现由 Rust 提取并返回规范化 E.164 MSISDN，与 Iteration 384 已迁入 Rust 的短信身份和有序 `Service-Route` 投影共用 authenticated IMS codec UDS。Go 生产路径已删除号码提取副本；旧逻辑仅保留在测试中作为差分基准。兼容既有 `tel:`、`sip:`、`sips:` 身份、百分号解码及号码格式化规则。Go 仍负责 SIP socket/事务、物理 AKA、IPsec 和 IMS 会话生命周期。

完整离线迁移门禁通过：[iteration-385-migration-gates.log](../build/optimization/iteration-385-migration-gates.log)。Rust workspace **1,962 passed / 6 ignored**（daemon **1,390 passed / 3 ignored**），Go 全包 test/vet、Swift BLE 回归和现有跨语言 parity 通过；API 契约 `--diff`、审计器 **43 tests**、Rustfmt 与 `git diff --check` 通过。门禁日志 SHA256：`f9c2a0a3f9b8722a1e6e80bfd78c35beeadec363f7b555aca3a3e7c81f26563c`。未进行实卡、运营商、短信或通话验收；未生成 DMG。

P3 +0.025，累计 **2.340 / 8.0**；项目累计 **16.820 / 47.5 = 35.4105%**（显示 **35.4%**，两位小数 **35.41%**，估算误差约 ±3 个百分点）。本轮仅计 MSISDN 提取生产策略迁移，不代表 IMS socket、AKA/IPsec 或 VoWiFi 会话已由 Rust 接管。
