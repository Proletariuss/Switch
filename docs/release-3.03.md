# Switch 3.03 / build 303 发布记录

日期：2026-10-12

按用户明确要求从 Switch 当前源码构建发行版，包含至 Iteration 400 的源码。使用 Xcode 27 构建 arm64 应用，最低系统版本 macOS 27.0；App 内含 Rust 与 Go 后端。

- DMG：[Switch-3.03-macOS27.dmg](../../dist/Switch-3.03-macOS27.dmg)
- SHA-256：`de2d3af97a6ada152803ac3f79db3d202dcb14f2166c1c3e42e95d9b59ac38ae`
- 大小：52,872,847 bytes
- 版本：3.03 / build 303
- 架构与最低系统：arm64 / macOS 27.0
- 验证：`hdiutil verify` 通过；挂载后 Info.plist 版本、构建号和最低系统版本正确；主程序为 arm64；`codesign --verify --deep --strict` 通过；发行 DMG 与本地归档 SHA-256 一致。
- 签名：本机 ad-hoc 签名；未配置 Developer ID 身份或公证配置，未做 Apple notarization。

此安装包对应混合 Go/Rust 后端阶段，不代表全 Rust 迁移完成，也未完成运营商实机验收。本次仅构建并检查包，未操作读卡器、号卡、短信或拨打电话。
