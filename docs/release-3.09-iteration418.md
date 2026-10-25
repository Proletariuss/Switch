# Switch 3.09 / build 309 — Iteration 418

- DMG：`/Users/proletarius/Desktop/Switch/dist/Switch-3.09-macOS27.dmg`
- 本地归档：`/Users/proletarius/Desktop/Switch/dist/archive/Switch-3.09-macOS27.dmg`
- 大小：53,105,849 bytes
- SHA-256：`078f9d2e8028d72e1fde0bb2ee859a6ea5340048e3f1f91df0eaea41d422edb7`
- 平台：Apple Silicon arm64，最低 macOS 27.0
- 签名：本机 ad-hoc；未执行 Developer ID 公证
- 验证：Iteration 418 全量迁移门禁通过；Release 构建成功；`codesign --verify --deep --strict` 与 `hdiutil verify` 通过；只读挂载核对版本 3.09/build309、最低系统版本及 arm64 主程序；dist 与 archive 的 SHA-256 一致。
- 构建日志：`/Users/proletarius/Desktop/Switch/build/optimization/release-3.09-iteration418.log`
- 迁移门禁日志：`/Users/proletarius/Desktop/Switch/build/optimization/iteration418-migration-gates.log`，SHA-256 `dc40ac2193dac735c25a6957665cad36cd262bfdc6865b8da5be9416903a358a`

该版本包含 Iteration 418 的 Rust codec 能力探测与 SDP 协商改动。它仍是混合后端，不代表全 Rust 目标完成；本轮未启动 App、连接读卡器、操作号卡、发送短信/拨号或做运营商实机验收。
