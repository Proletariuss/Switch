# Iteration 402：IMS USSD XML 编码接入 Rust

2026-10-12

读卡器 VoWiFi 的生产 IMS session 现在通过现有认证 `/api/v2/ims/validate-register-response` endpoint 调用 Rust `encode_ussd_xml` operation。Rust 校验 1–182 个 Unicode scalar、拒绝 NUL/CR/LF，并生成与 Go `encoding/xml` 相同的 `application/vnd.3gpp.ussd+xml` wire bytes，包括 UTF-8 声明和 `&`, `<`, `>`, `"`, `'` 的转义。Go adapter 对 HTTP 状态、base64 长度、XML 根、语言、错误字段及回读文本做边界校验。

本轮只迁移 XML 文本编码。Go 仍负责 multipart/SDP 请求体拼装、USSD SIP request/response、XML 解码、对话队列、计时、取消和会话生命周期；离线测试没有声称 IMS/运营商实机可用。测试隔离构造的 `Service` 在未注入 Rust builder 时保留纯 Go fixture fallback；生产 reader-backed IMS session 明确注入 Rust builder。

验证：Rust IMS XML codec 2 项、Rust daemon operation 1 项、Go imscore USSD 与 native bridge 定向测试通过；同两处相关 Go 测试的 `-race` 通过。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace 1,410 passed / 3 ignored，Go vet/全包测试、Swift BLE 回归和跨语言 parity 通过，carrier parity 重放 2,722 cases。契约 `--diff` 通过，346 method/path（Rust 286 / Go 60）、127 Swift 调用点、361 个审阅 scope；无新增 HTTP method/path。格式检查及 `git diff --check` 通过。门禁日志 [`iteration-402-migration-gates.log`](../../build/optimization/iteration-402-migration-gates.log)，SHA-256 `3fcf4e13d860152965f5d9f04cdf07b381415d344afd71217bdc914444c89587`。

P3 +0.025，累计 **2.540 / 8.0**；全项目 **17.245 / 47.5 = 36.3%**（精算 36.31%，估算误差约 ±3 个百分点）。无真实卡、运营商、IMS session、SMS 或 USSD 实机验收；不代表 Go 后端已退出。本轮记录时 3.04/build304 尚未包含本轮代码；随后已按用户要求生成并核验 3.05/build305 DMG，其中包含本次源码。
