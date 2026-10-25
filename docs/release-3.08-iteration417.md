# Switch 3.08 / build 308 — Iteration 417 snapshot

日期：2026-10-12。按用户要求从 `/Users/proletarius/Desktop/Switch/src` 当前源码构建；包内包含 Iteration 417 移除 Go 本地 SDP 构造 fallback 的改动。

- DMG：`/Users/proletarius/Desktop/Switch/dist/Switch-3.08-macOS27-iteration417.dmg`
- 本地归档：`/Users/proletarius/Desktop/Switch/dist/archive/Switch-3.08-macOS27-iteration417.dmg`
- 大小：53,038,215 bytes
- SHA-256：`ececca0138307484d66c49cdd7845257d90c61ff6feefa041946af60621a39fc`
- 版本/构建：3.08 / 308；arm64；最低 macOS 27.0
- 验证：全量迁移门禁通过；`codesign --verify --deep --strict` 通过；`hdiutil verify` 通过；归档副本 SHA-256 与发行包一致。
- 门禁日志：`/Users/proletarius/Desktop/Switch/build/optimization/dmg-preflight-iteration417-migration-gates.log`（SHA-256 `0f87b451354c731c5c1c705ee71044222398f6864518d4cdba42c8f0887823d3`）。
- 签名：本机 ad-hoc 签名；未执行 Apple 公证。

此安装包仍是 SwiftUI + Go/Rust 混合后端，不代表全 Rust 改造完成。没有启动 App、操作真实读卡器或号卡、发送短信、拨号或执行运营商实机验收。
