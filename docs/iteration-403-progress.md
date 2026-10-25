# Iteration 403：USSD SDP 与 multipart 请求体构造迁入 Rust

2026-10-12

读卡器 VoWiFi 生产 USSD 发起现在通过现有 authenticated IMS codec endpoint 调用 Rust 一次性生成 USSD XML、signaling-only SDP 和 `multipart/mixed` body。Rust 使用系统熵生成每请求 MIME boundary，并校验本地 IP 地址和既有 USSD 文本长度/字符约束；Go adapter 验证响应状态、base64/body 上限、multipart media type、boundary 起止边界后将字节交给原 SIP 请求构造。生产 reader-backed session 注入 Rust builder。Go 保留 SIP headers/dialog、事务、连接写入、响应解码、队列、取消和 IMS 生命周期；孤立单测的旧 Go body helper 仍作 fixture fallback，不是生产后备。

无新增 HTTP method/path。Rust IMS crate、daemon operation、Go builder bridge 和 IMS config 注入都有定向测试；Go 使用 `mime/multipart` 解析构建体，验证 SDP 与 USSD 两个 part、头和边界。Rust USSD 模块 4 tests、daemon operation 1 test 通过；Go imscore/native bridge 定向测试及同两处 race 检查通过。完整 `scripts/test-migration-gates.sh` 通过，Rust daemon 1,411 passed / 3 ignored；Go 全包/vet、Swift BLE 回归和跨语言 parity 通过。契约 census `--diff` 通过：346 method/path（Rust 286 / Go 60）、127 Swift call sites、361 reviewed scopes；没有新 route。`git diff --check` 通过。日志 [`iteration-403-migration-gates.log`](../../build/optimization/iteration-403-migration-gates.log)，SHA-256 `9204f30dfa77b53b88122d22a7293302a6b9b54084aa66c55e9a7b5e3a8b445e`。

P3 +0.025，累计 **2.565 / 8.0**；全项目 **17.270 / 47.5 = 36.36%**（显示 36.4%，估算误差约 ±3 个百分点）。没有真实读卡器、运营商 IMS/USSD、SMS 或通话验收；本轮未生成 DMG。3.05/build305 包包含至 Iteration 402，不含本轮源码。
