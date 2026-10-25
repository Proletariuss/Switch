# Switch 3.13 / Build 313

- DMG: [`Switch-3.13-macOS27.dmg`](../../dist/Switch-3.13-macOS27.dmg)
- 本地归档：[`Switch-3.13-macOS27.dmg`](../../dist/archive/Switch-3.13-macOS27.dmg)
- 架构：arm64；最低 macOS 27.0
- SHA-256：`c23606c174ffbbc78a1fb7c14057f68445ed2b8b5c795b0757a8ad031d3a07d5`
- 大小：53,145,239 bytes
- 签名：本机 ad-hoc；未进行 Developer ID 公证
- 校验：Iteration 433 全量迁移门禁通过；Release 编译成功；挂载后的 App 通过 `codesign --verify --deep --strict`；`hdiutil verify` 通过；App、`switchd-rs`、`switchd` 均为 arm64；DMG 与 archive 哈希一致；内嵌版本/build 为 3.13/313。
- 未开展实体读卡器、模块或运营商行为验收。
