# Iteration 446：Rust 原生短信 owner 绑定兼容订阅身份

日期：2026-10-13

## 改动

- 原生 VoWiFi session context 在同一已授权 reader lease 快照中推导短信归档 `subscription_id`，不接受 HTTP/UI 请求中的卡身份。
- Rust 使用与旧 Go `deviceSMSSubscriptionID` 相同的版本化 SHA-256 输入格式，并对 ICCID/IMSI 执行相同长度、数字与全零校验。
- `NativeSmsSetup.subscription_id` 改为显式 `Option<String>`：`None` 使用 context 的租约绑定身份，`Some("")` 保持无订阅标识来源，避免把 N 待或其他来源静默改标。

## 验证与范围

- `native_vowifi_session::tests`：**52 passed / 0 failed**。
- Rust daemon 非测试 `cargo check --locked -p proletariusd` 通过；保留 17 项既有 dormant-code warning。
- `rustfmt --check`、`git diff --check` 通过。
- 当前 API 契约盘点与记录一致：354 contracts（Rust 293 / Go 61）、127 Swift 调用点、378 审阅范围；迭代契约差异检查通过。
- 该输入仍在未接入生产启动路径的 native owner 中，VoWiFi、IMS、SMS/call gates 均未开放，因此不增加加权迁移进度。总体 **37.99%（18.045/47.5）**；Go 退出仍有 6 类阻塞、266 个生产 Go 文件。
- 未操作实体卡、SIM、短信或运营商网络。3.15/build315 iteration445 DMG 在本轮代码更改前生成，不包含本轮源码变更；本轮未生成新的 DMG。

## 下一步

继续把 authoritative REGISTER 安全端点、IMS MESSAGE 路由/事件消费及 call runtime 所需输入接入同一个 generation owner；在 SMS 和 call runtime、错误/清理语义均有离线闭环前，保持生产启动门禁关闭。
