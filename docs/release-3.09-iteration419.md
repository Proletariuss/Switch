# Switch 3.09 / build 309 — Iteration 419

- DMG：`/Users/proletarius/Desktop/Switch/dist/Switch-3.09-macOS27-iteration419.dmg`
- 大小：54,162,323 bytes
- SHA-256：`aed6d69831f6b2d8b73e1fb19f806805ae22b5d23c11e2e7bfe26ddf20cee71d`
- 平台：Apple Silicon arm64，最低 macOS 27.0
- 签名：本机 ad-hoc；未执行 Developer ID 公证
- 验证：Release 构建成功；`codesign --verify --deep --strict` 与 `hdiutil verify` 通过。镜像使用唯一 Iteration 文件名，既有 3.09 DMG 未覆盖。
- 构建日志：`/Users/proletarius/Desktop/Switch/build/dmg-iteration419-app-build.log`，SHA-256 `359356437a1f17dabce5830f78313fa9332acb259ec97e07fe6c0921b7ae2dbf`
- 全量迁移门禁：[iteration419-migration-gates.log](../build/optimization/iteration419-migration-gates.log)，SHA-256 `f864157018c3edf9fa4577f22f9d1e7358aaa112521e85acb026e4c5b527a40f`

该包以当前 Iteration 419 源码构建，包含 Rust RFC 4733 DTMF 事件序列生成及 Go 到 Rust 的生产桥接。全量离线门禁和定向 race 测试通过；没有执行真实读卡器、SIM、运营商通话或短信测试。项目仍为 Rust/Go 混合后端，全 Rust 目标未完成。
