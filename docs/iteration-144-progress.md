# Iteration 144：短信转发配置竞态与 Telegram 错误诊断

日期：2026-10-08。全 Rust 后端目标持续实施；源码与验证材料均在 `/Users/proletarius/Desktop/Switch/`。

## 实施

修复短信转发设置页的配置同步竞态：如果 Bot 测试与启动或保存期间的配置同步重叠，旧逻辑会让第二个调用立即返回，随后用 Rust 后端的旧配置测试 Telegram。后端因此可能返回 `telegram_not_enabled`，而错误映射又把它折叠成截图中的 `provider_configuration_invalid`。现在测试调用会等待当前配置同步器处理最新配置 revision 并收到确认后，才请求测试通知。

同时修正 Telegram 错误诊断映射。后端只透传一组固定、已脱敏的 provider 错误码，包括未启用、网络失败、代理解析、Telegram HTTP 状态/API 拒绝及响应格式问题；不接受任意字符串回显。设置页将常见错误转为可操作的中文提示，因此 Bot Token/Chat ID 错误与系统代理/Telegram 服务错误不再混为“配置错误”。

短信中心的设备短信与 N 待消息仍由 Rust producer 在配置确认和耐久 handoff 后入队。本轮对两条入队通路做了合成回归；未读取用户 Token、短信正文或队列数据，也未向 Telegram/Bark 发送真实通知，未用读卡器发送短信。真实服务可达性与后续新短信推送仍需在用户当前 Bot/网络配置下做一次运行验证。

## 验证

- Swift 转发客户端并发回归：`scripts/test-sms-forwarding-client.sh` 通过，覆盖“配置请求挂起时触发测试”不会提前调用 provider。
- Rust 转发服务：20 项通过；设备短信 producer：14 项通过；N 待 producer：4 项通过；handoff 跨语言兼容通过。
- `scripts/test-migration-gates.sh`：通过；Rust workspace **844通过、0失败、3忽略**，Go 全仓测试/vet 和 HTTP/HEAD、载波、USB状态、射频及音频协议兼容门禁通过。
- P0 路由/调用清单一致：279条 method/path（Rust 201、Go 78，含31条 HEAD alias）、127个 Swift 调用点、226个审阅范围；迁移工具42项自测通过。Rust-only门禁仍因默认 Go fallback 和143条 Go 所有权/依赖路由未通过，继续作为未完成项。
- Release `Switch.app` **2.93/build293 arm64** 构建成功，深度/严格签名验证通过；内嵌 `switchd-rs` SHA-256：`86d26a6501bada3d8a0dc153c78b3be0e1dc1deb55340b61febee825cea28c98`。
- `git diff --check` 通过。未生成 DMG。

日志位于 `build/optimization/iteration-144/`：`migration-gates.log`、`release-build.log`、`forwarding-service.log`、`device-producer.log`、`nwait-producer.log`、`swift-forwarding-client.log` 和 `handoff-compat.log`。

## 进度

本轮是已有 Rust 短信转发能力的可靠性与诊断修复，没有新增迁移边界，因此不增加计划人周分数。全 Rust 后端仍为 **27.7%（13.145/47.5计划人周，估算误差约±3个百分点）**；P2A为 **68.36%（3.760/5.5）**，P2B为 **58.13%（4.650/8.0）**。真实 Telegram/Bark 服务推送、真实来信端到端、设备与长稳验收以及 Go 退出仍未完成。
