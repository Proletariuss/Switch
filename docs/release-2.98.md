# Switch 2.98 / 构建 298

2026-10-12，按用户要求在 Iteration 376 后生成 DMG。

## 发布范围

该包包含当前 Switch 源码截至 Iteration 376 的全部已纳入构建改动，包括 9eSIM B0/B1 认证集成、此前已完成的 UI/短信/VoWiFi 功能，以及 REGISTER 请求/响应的 Rust 编码与解析接管。该包仍是 SwiftUI + Go/Rust 混合后端，不代表全 Rust 目标完成；Go 后端可执行文件仍随 App 一同打包。

## 验证

- `scripts/test-migration-gates.sh`：Rust workspace **1,934 passed / 6 ignored**（daemon **1,383 passed / 3 ignored**）；Go 全包测试与 vet、Swift BLE 证明/manager 回归、协议 parity 通过。
- API 契约 `--write-inventory` / `--diff` 与检查器 43 项测试通过。
- Release 构建成功；只读挂载后核验 App 版本 **2.98**、构建 **298**、bundle ID `com.proletarius.switch`；App 与两套后端均为 arm64。
- `codesign --verify --deep --strict` 与 `hdiutil verify` 通过；本机 ad-hoc 签名，未执行 Apple 公证。
- 发行 manifest 大小与 SHA-256 与产物一致。编译期间有 macOS 27 API 弃用提示和现有 dead-code warning，不影响构建通过。

文件：`/Users/proletarius/Desktop/Switch/dist/Switch-2.98-macOS27.dmg`

大小：53,373,926 字节。

SHA-256：`1059caa8f7d251542740e82d7e7637256c6f7be6bb8c5c944ec339a059be34a3`。

未安装或启动 App；没有操作实体读卡器/号卡、发送短信、拨号或做运营商 VoWiFi 验收。
