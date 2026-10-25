# Iteration 473：移除 Go AT-console 兼容执行残留

日期：2026-10-13。正常 `POST /api/at` 请求已由 Rust gateway 路由至 Rust module-radio。移除 Go 显式兼容启动路径中的公开 `/api/at` registration/handler、AT parser/result wrapper、没有生产调用者的 `nativeRadioDiagnostic` client，以及仅服务于这些 Go 实现的测试。Rust handler继续执行命令验证、飞行模式蜂窝操作阻断、VoWiFi 通道排他、模块执行结果分类；执行结果不确定时仍要求用户先核对设备，不会自动重放。

全量离线验证：Rust 定向 AT console 测试 **5 passed**；Go `proletarius-macos` package tests 通过；完整迁移门禁通过，Rust workspace **1,480 passed / 0 failed / 3 ignored**，Go 全仓 test/vet、Swift BLE、2,726 carrier replay、USB/radio、HTTP method+HEAD、IMS/SDP/AMR/MESSAGE/USSD parity通过。合同 `--diff`、Rustfmt、`git diff --check` 通过。合同 census 354（Rust295/Go59、51 HEAD aliases）、127 Swift HTTP calls、381 review scopes。门禁日志 SHA-256：`abad3d9175e05132ad15c7bf0d685e37da640a80b5765fd0b7e64a4112c9ece6`。结构退出审计仍6类阻塞，生产 Go 文件265个，见[审计日志](../build/reports/iteration-473-go-exit-audit.log)。

这轮删除的是已经由 Rust 接管的兼容副本，不重复计入功能迁移分；整体保持 **38.09%（18.095/47.5，误差约±3个百分点）**。实机、SIM 和运营商验收未进行；9eSIM `6982` 仍排除。按用户要求，已基于本轮源码重新构建 Release 并生成 [Iteration473 DMG](../dist/Switch-3.15-macOS27-iteration473-20261013.dmg)：版本 3.15/build315，arm64，51 MB；`hdiutil verify`、包内 App deep/strict 签名、版本与 Rust daemon 哈希核对通过。镜像 SHA-256：`695ced5c13638e4972863363aeb7ea4ed29c38f820946086b3fbb399ffc74e14`。本机 ad-hoc 签名，未公证。此发行包仍包含 Go `switchd`，所以不是全 Rust 成品；全 Rust 退出审计仍有6类构建/运行时阻塞。
