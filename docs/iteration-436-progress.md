# Iteration 436：通话模式恢复设备绑定策略迁入 Rust

日期：2026-10-13

将恢复备份的设备绑定校验从 Go 迁入 Rust 备份 owner。Go 仍读取 USB/IMEI 实时事实，Rust 重新读取并验证所选备份，并校验其模块 IMEI 与当前模块一致、USB VID/PID 相符；不匹配沿用既有错误语义。校验失败不会继续到硬件写入。Go 在硬件信任边界仍保留通话/模块状态复核、reservation 和恢复执行。

新增精确认证内部端点 `POST /api/internal/call-mode/backups/validate-restore`，请求体限制 1 KiB。Rust 离线测试覆盖匹配、IMEI、VID、PID 不匹配；Go 适配器测试覆盖请求身份绑定和 Rust 错误透传。

完整迁移门禁通过：Rust workspace 2068 passed / 0 failed / 6 ignored；Go 全包 test/vet、Swift BLE、2722 条 carrier replay、IMS/USSD parity 均通过。API 契约 diff 通过，迁移门禁日志 SHA-256：`1cb8916a29a25013cdcb6a04079daca82e32b0d591f935c48117376e4129d7d8`。API census 354 method/path（Rust 293 / Go 61），127 Swift 调用点、378 审阅范围。Go 退出审计仍有 6 类阻塞、266 个生产 Go 源文件。未进行实体设备或运营商验收。

P2B +0.025，累计 **18.045 / 47.5 = 37.99%**（估算误差约 ±3 个百分点）。当前 DMG 3.14/build314 冻结于 Iteration 435，不包含本轮源码；按目标不重打 DMG。
