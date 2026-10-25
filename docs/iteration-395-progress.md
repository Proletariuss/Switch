# Iteration 395：Rust 接管来电 INVITE 接纳策略

Rust tracker 现在处理生产来电 INVITE 的接纳、重复请求/重传分类、SIP CSeq 防旧事件和 Trying/Ringing/Connecting/Active/Ended 阶段。Go 通过现有受认证的呼叫策略路由调用 Rust，并依据返回策略执行 SIP 回包及媒体副作用；Go 仍负责 SIP socket、报文 I/O、媒体与通话生命周期。本轮未新增公开 API 路径。

定向 Rust tracker、daemon route、Go bridge/runtime 测试通过；完整 `scripts/test-migration-gates.sh` 通过：[Iteration 395 门禁日志](../build/optimization/iteration-395-migration-gates.log)，SHA-256 `1a54238e3815b08e11cc1c99b710f00bba837ed5aa2be1528f80949255f877f7`。契约 `--diff` 与 43 项审计自测通过，清单为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、360 审阅范围。

P5 +0.025，累计 **0.125 / 8.0**；项目累计 **17.070 / 47.5 = 35.9368%**（显示 35.9%，两位小数 35.94%，估算误差约 ±3 个百分点）。未进行读卡器、eSIM、短信、通话或运营商实机验收；9eSIM `6982` 仍按用户要求排除。

本轮按用户单独请求生成 Release DMG：**Switch 3.00 / build 300**，arm64，最低 macOS 27.0，本机 ad-hoc 签名。安装包内版本与签名已核对，`hdiutil verify` 通过。SHA-256 `391b59af32db20dcfb83c8aeac6b97c607023e250aa85b6d93c6c5c7eb522aef`。此安装包包含 Iteration 395 源码，但全 Rust 目标仍未完成。
