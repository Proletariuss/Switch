# Switch 3.06 / build 306

本版在原有生产 reader-backed IMS 会话中，将 USSD 请求 XML、signaling-only SDP 与 multipart body 构造，以及 USSD XML 响应字段解码迁入 Rust。Go 继续负责 SIP 对话、multipart 提取、网络 transport 和生命周期。没有新增公开 API 路由。

构建前完整迁移门禁通过：Rust workspace 1,412 passed / 3 ignored；Go 全包测试与 vet、Swift BLE 回归、载波/USB 状态/无线电/HTTP 方法 parity 均通过。该结果是离线自动化验证，不代表真实读卡器、运营商 USSD、短信或通话验收。

DMG 使用本机 ad-hoc 签名，不含 Developer ID 公证。
