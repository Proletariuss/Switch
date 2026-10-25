# Iteration 211：Rust IMS 注册流的 SIP 解析边界

日期：2026-10-09。所有改动只在 `/Users/proletarius/Desktop/Switch/` 内；本轮没有访问设备或运营商。

## 变更

新增 `proletarius-vowifi::sip_session_runtime::RegisteredSipRuntime`，在已有 `RegisteredImsSession` 上提供单一可变 owner 的类型化读取、主连接/Port-S 连接归属、原连接事务回应和主 IMS 连接写入。事件解析使用有上限的 Rust SIP parser；额外尾随字节、缺失/重复的 Via/From/To/Call-ID/CSeq、非法状态码及响应头注入会被拒绝。解析后的 SIP body 由 `Zeroizing<Vec<u8>>` 持有，事件类型不实现 `Debug`。

此模块尚未被 `switchd-rs` 的生产 VoWiFi session 或公开 API 调用。它没有实现 MESSAGE/RP-ACK 持久化顺序、Call-ID/CSeq 响应订阅、OPTIONS/NOTIFY 策略、电话事件与媒体，也没有启动/停止 actor。因此 Go 仍拥有 IMS 生产事件循环，所有 native-session readiness gate 保持关闭；本轮不调整全 Rust 目标百分比。

## 验证

- `cargo +1.98 test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi`：152 个单测、2 个载波集成测试通过。
- `cargo +1.98 check --offline --manifest-path rust/Cargo.toml -p proletariusd`：通过；输出16条既有 dead-code warning，本轮新增代码没有额外 warning。
- `cargo +1.98 fmt --manifest-path rust/Cargo.toml --all -- --check` 与 `git diff --check` 通过。

全项目仍为 **31.4%（14.920/47.5计划人周等值）**，误差约±3个百分点。下一步应将类型化事件接到 keyed session actor，并先完成可持久化的入站短信/RP-ACK和有界响应分派，再接入呼入/呼出事务与关停顺序。没有生成 App 或 DMG；没有进行真实呼叫、短信或卡片操作。
