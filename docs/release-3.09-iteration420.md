# Switch 3.09 / build 309 — Iteration 420

- DMG：`/Users/proletarius/Desktop/Switch/dist/Switch-3.09-macOS27-iteration420.dmg`
- 大小：53,111,845 bytes
- SHA-256：`59007726d0bee146d445534f3c4a5382a25cc49d571e111d5c0fa8ed2fe9fb9a`
- 平台：Apple Silicon arm64，最低 macOS 27.0
- 签名：本机 ad-hoc；未执行 Developer ID 公证
- 验证：Release 构建成功；DMG `hdiutil verify`、挂载后的 App `codesign --verify --deep --strict` 均通过；确认镜像包含 `switchd` 与 `switchd-rs`。
- 构建日志：`/Users/proletarius/Desktop/Switch/build/dmg-iteration420-app-build.log`
- 迁移门禁：[iteration420-migration-gates.log](../build/optimization/iteration420-migration-gates.log)，SHA-256 `0fb82915285355f5cd118a83235512f447ab1c562c16b93fd8dfd3aeb50e95c7`

该包包含 Iteration 420：Rust 按键事件策略生成完整 RFC 4733 DTMF RTP 数据报及后续计数器；Go 验证后仍通过现有 UDP socket 按节奏发送。未进行真实读卡器、SIM、运营商通话或短信测试。项目仍为 Rust/Go 混合后端，全 Rust 目标未完成。
