# Iteration 364：IMS Digest Authorization 头格式化迁入 Rust

IMS AKA 摘要计算已在 Iteration 362 接入 Rust，但生产回调仍把 Go 生成的 `digest.Credentials` 头作为最终 Authorization 值。本轮将完整 `Digest ...` 头生成（username/userhash、realm、nonce、URI、algorithm、opaque、cnonce、qop、nonce count、response 与 AUTS）迁入 Rust IMS digest handler。Go 仍执行物理 AKA、选择 qop/cnonce，并校验返回头解析出的字段与挑战、请求及 AUTS 完全一致后才发 REGISTER；Rust 不接管 SIP parser、网络 stream 或 IMS 会话生命周期。

为复用生产实现，AKAv1-MD5 兼容 `voiceclient` 的回调适配也转入共享 Rust response/Authorization 契约。未提供 callback 的显式兼容路径仍由 Go 计算，不计为 Go 退出。用户排除的 9eSIM `6982` 未处理。

验证：Rust digest handler 定向测试通过；Go `simauth`、`voiceclient`、`imscore` 与 macOS daemon 定向测试通过；完整 `scripts/test-migration-gates.sh` 通过，Rust daemon **1,368 passed / 3 ignored**，Go 全包测试/vet、Swift BLE proof、协议 parity 通过；迁移契约扫描 diff 通过，343 method/path（Rust 283 / Go 60）、127 Swift 调用点、353 审阅范围。完整日志：[migration gates](../build/optimization/iteration-364-migration-gates.log)。未进行实机 SIM、短信或通话测试，未生成 DMG。

本轮 P3 +0.025 计划人周等值；P3 为 **1.840 / 8.0 = 23.0%**，全项目 **16.320 / 47.5 = 34.3579%**（显示 **34.4%**，两位小数 **34.36%**；估值误差约 ±3 个百分点）。Go 物理 AKA、qop/cnonce选择、SIP构造的其他分支、SIP stream/session、IKE/IMS在线生命周期与语音仍未退出。
