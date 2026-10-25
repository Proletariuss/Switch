# Iteration 522: ESP 与初始 CHILD SA 独立向量核对

2026-10-18。延续 [Iteration 521](iteration-521-simworld-udp-flow.md) 的 SIM World `esp_sent=8、esp_received=0` 诊断，先在离线环境核对客户端构造 ESP 报文及密钥方向。没有启动读卡器会话、切卡、修改代理规则、发送短信、拨号、替换已安装 App 或生成 DMG。

为避免仅用本项目的加解密函数互相验证，使用独立的 Node/OpenSSL AES-GCM、AES-CBC、HMAC-SHA256 实现生成固定合成向量；Rust 测试比较**完整 ESP 线格式字节**并解码独立报文：AES-128-GCM/IPv4、AES-256-GCM/IPv6，以及 AES-256-CBC/HMAC-SHA256-128/IPv4。向量分别使用不同的密钥、salt、IV、SPI 与序号字节，覆盖 GCM 显式 IV、SPI/序号认证附加数据、ESP 尾部、CBC 填充和截断 ICV。另以独立 HMAC-SHA256 PRF+ 向量核对初始 CHILD SA 的 128 字节密钥材料及两方向的加密键、完整性键顺序。相关测试位于 `rust/crates/proletarius-ike/src/esp.rs` 和 `rust/crates/proletarius-ike/src/keymat.rs`。

这一排列符合 [RFC 4106](https://www.rfc-editor.org/rfc/rfc4106.html) 的 ESP GCM 线格式及 [RFC 7296 §2.17](https://www.rfc-editor.org/rfc/rfc7296.html) 的初始 CHILD SA `prf+(SK_d, Ni | Nr)` 和密钥方向约定。当前 `3_hk.json` 用户模板请求 `aes256-sha256` ESP 提案，但本轮**没有记录 SIM World 实际协商结果**；合成向量通过并不能证明实卡会话的 SPI、键、内层流量选择器、代理出口或 ePDG 实际接受了该 ESP。

验证：专项 IKE crate 49 个单测通过、0 失败；包含全部新测试的最终完整 Rust workspace 为 3037 个单测通过、0 失败、1 忽略。`cargo build --locked -p proletariusd`、迁移契约检查（294/294 Rust HTTP owner）、格式与 diff 检查通过。完整工作区日志见 `build/reports/iteration-522-rust-workspace-final.log`，最终 IKE 日志见 `build/reports/iteration-522-ike-final.log`，构建日志见 `build/reports/iteration-522-rust-build.log`。本轮只增加测试与记录，未改生产 ESP/IMS 路径。

下一步需要实际协商的 CHILD SA 套件和脱敏 SPI/方向信息，以及代理出口或 ePDG 侧对 UDP 4500 ESP 的接收证据，才能继续定位入站为零的原因。现有证据不足以宣称 SIM World IMS 注册、短信或通话已通过。
