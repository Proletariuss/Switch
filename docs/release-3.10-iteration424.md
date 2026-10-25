# Switch 3.10 / build 310 — Iteration 424

DMG：`../dist/Switch-3.10-macOS27.dmg`

SHA-256：`984eef6749d5bee655017864b1aa899714c73037754584dfdf732f061b5a61a2`

大小：53,119,568 bytes。发布脚本完成 Release build；`hdiutil verify` 通过；从 DMG 挂载后验证 App 签名、arm64 架构及 `CFBundleShortVersionString=3.10` / `CFBundleVersion=310`。归档副本在 `../dist/archive/Switch-3.10-macOS27.dmg`，与交付镜像 SHA-256 相同。

使用本机 ad-hoc 签名，未进行 Developer ID 公证；首次打开时 macOS 仍可能要求用户允许。此阶段包包含 Iteration 424 来电挂断策略/CSeq 迁移。全 Rust 迁移尚未完成，7类 Go 依赖和264个生产 Go 文件仍未退出；未执行 SIM、读卡器或运营商实机测试。
