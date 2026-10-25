# Switch 3.09 / build 309 — Iteration 422

- DMG：`/Users/proletarius/Desktop/Switch/dist/Switch-3.09-macOS27-iteration422.dmg`
- 大小：53,118,453 bytes
- SHA-256：`911de23e1212cda6aff529adec3beb8558e08972014916c6932ff8010391dac4`
- 平台：Apple Silicon arm64，最低 macOS 27.0
- 签名：本机 ad-hoc；未执行 Developer ID 公证
- 验证：`hdiutil verify` 与挂载后的 `codesign --verify --deep --strict` 通过；包内版本 3.09/build309，App、Go 后端和 Rust 后端均为 arm64。
- 迁移门禁：[iteration422-migration-gates.log](../build/optimization/iteration422-migration-gates.log)，SHA-256 `c2822d953a2f424cc71e7eb36b04f87bda5384c2242f6a200f25d5e9e4993d6a`

该包包含 Iteration 422：生产外呼 PRACK/BYE 的本地 CSeq 由 Rust tracker 分配并由 Go 校验后用于 SIP 执行。Go 仍负责请求构造/写入、媒体与会话清理，以及初始 INVITE、入站呼叫和策略 IPC 失败时的紧急清理路径。没有进行实体读卡器、SIM、运营商短信或通话测试；全 Rust 目标仍在推进。
