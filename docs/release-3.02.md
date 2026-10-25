# Switch 3.02 / build 302 发布记录

日期：2026-10-12

本次按用户要求从 Switch 源码树构建发行版，包含至 Iteration 399 的代码。使用 Xcode 27.0 构建 arm64 应用，最低系统版本 macOS 27.0；App 内含 `switchd` 与 `switchd-rs`。

- DMG：[Switch-3.02-macOS27.dmg](../../dist/Switch-3.02-macOS27.dmg)
- SHA-256：`fe585a7f43d0f0d7787bd54b364dfdbee97266430d23b95a4ac1d28ce91f485d`
- 大小：52,866,872 bytes
- 验证：`hdiutil verify` 通过；挂载后 Info.plist 显示版本 3.02/build 302；主程序为 arm64；`codesign --verify --deep --strict` 通过；归档 DMG 与发行 DMG SHA-256 一致。
- 签名：本机 ad-hoc 签名；未配置 Developer ID 身份或公证配置，未做 Apple notarization。

此安装包对应混合 Go/Rust 后端阶段，不代表全 Rust 迁移或运营商实机验收完成。本次仅构建并检查包，未操作读卡器、未发送短信或拨打电话。
