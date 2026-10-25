# Switch 3.14 / Build 314

- DMG: [`Switch-3.14-macOS27.dmg`](../../dist/Switch-3.14-macOS27.dmg)
- 本地归档：[`Switch-3.14-macOS27.dmg`](../../dist/archive/Switch-3.14-macOS27.dmg)
- SHA-256：`77a1e39ce5c78faddd7b9b11a4cea9932b99f773d04a1ac870368c0a560d1203`
- 架构：arm64；最低系统版本：macOS 27.0
- 签名：本机 ad-hoc；未使用 Developer ID 签名或公证
- 包内核验：Info.plist 为 3.14/314；Switch、Rust daemon、Go adapter 均为 arm64；App `codesign --verify --deep --strict` 通过；DMG `hdiutil verify` 通过；归档副本 SHA-256 与正式 DMG 一致。
- 源码：包含至 Iteration 435 的更改。Iteration 435 全量离线迁移门禁日志：[`iteration435-migration-gates.log`](../../build/reports/iteration435-migration-gates.log)，SHA-256 `fa8058cf9c1fa472a57c2001848a6fa8f9acd0c88aed82d3e8f4790dcb9586fe`。
- 限制：未进行真实读卡器、SIM、蓝牙、模块或运营商实机验收。
