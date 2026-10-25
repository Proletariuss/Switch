# Iteration 471 源码 DMG 重建记录

日期：2026-10-13。按用户明确要求，从 `/Users/proletarius/Desktop/Switch/src` 当前源码重新构建，未修改版本号，产物为 **3.15 / build 315 / arm64**：

[`Switch-3.15-macOS27-20261013-latest.dmg`](/Users/proletarius/Desktop/Switch/dist/Switch-3.15-macOS27-20261013-latest.dmg)

DMG 大小约 51 MiB，SHA-256：`c3b64ad892b6fd957dba340694e6c9d26ddc0110a5fa372dc11d61d802b72de5`。打包脚本的 `hdiutil verify` 通过；从镜像只读挂载后，`codesign --verify --deep --strict` 通过，Info.plist 显示版本 3.15、build 315，Swift App、Rust daemon、Go daemon 均为 arm64。镜像内 Rust daemon 与 `dist/Switch.app` 中对应文件的 SHA-256 一致。该构建使用本机 ad-hoc 签名，未作 Apple Developer ID 公证。

验证：`scripts/build.sh` Release 构建成功；`cargo +1.98.0 fmt --all -- --check` 通过；`scripts/test-migration-gates.sh` 完整通过，Rust workspace **1,477 passed / 0 failed / 3 ignored**，Go 全仓 vet/test、Swift BLE 回归（38 组）、2,726 项 carrier replay 及 USB/radio/HTTP/IMS/SDP/AMR/MESSAGE/USSD/dial-target/rejection parity 均通过。没有连接或操作读卡器、SIM、短信、电话或运营商网络，因此本记录不代表硬件或运营商实测通过。

本次只重建交付包，不计入全 Rust 后端迁移进度；项目仍为 **38.04%（18.070/47.5，估算误差约 ±3 个百分点）**。全 Rust 目标未完成，DMG 不应被理解为迁移完成版。
