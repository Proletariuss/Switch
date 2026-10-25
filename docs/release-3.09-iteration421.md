# Switch 3.09 / build 309 — Iteration 421

- DMG：`/Users/proletarius/Desktop/Switch/dist/Switch-3.09-macOS27-iteration421.dmg`
- 大小：53,116,211 bytes
- SHA-256：`f9c9cfb54dfd9f5e4b3b1ec070409e2d57f865cbd4a5c86e97d3e8a9185b6a2a`
- 平台：Apple Silicon arm64，最低 macOS 27.0
- 签名：本机 ad-hoc；未执行 Developer ID 公证
- 验证：Release 构建成功；DMG `hdiutil verify`、挂载后的 App `codesign --verify --deep --strict` 均通过；确认包内 `switchd` 与 `switchd-rs` 均为 arm64。
- 迁移门禁：[iteration421-migration-gates.log](../build/optimization/iteration421-migration-gates.log)，SHA-256 `58046281cc3afaace076d2ad695d51826ba7f3d46207cd1ab36234844548a437`

该包包含 Iteration 421：外呼挂断动作（振铃期 CANCEL、通话期 BYE、重复请求忽略）由 Rust 根据呼叫 tracker 决策；Go 仍负责 SIP/socket 写入、CSeq、ACK 顺序、媒体与会话清理。没有进行真实读卡器、SIM、运营商通话或短信测试；全 Rust 后端目标仍在推进中。
