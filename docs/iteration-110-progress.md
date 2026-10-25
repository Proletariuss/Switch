# Iteration 110 — 遗留 UAC 状态接口迁入 Rust

日期：2026-10-08。继续执行[全 Rust 后端改造目标](optimization/2026-10-04-all-rust-backend-plan.md)。所有改动留在 Switch；未操作真实模块或号卡，未生成 DMG。

Rust 接管 `GET /api/voice/enabled` 和已停用的 `POST /api/voice/enable`。Go 删除对应 handler、启动缓存和状态文件写入。GET 通过 Rust modem actor 执行固定 `AT+QCFG="usbcfg"?` 查询；失败时回退已有 `voice-enabled.json`。POST 保持 1 MiB 请求体上限、拒绝未知字段/尾随 JSON，并返回原 409 停用提示。没有 USB composition 写入或模块重启。

验证：Rust daemon 测试 **719 通过、3 忽略、0 失败**；Go `cmd/proletarius-macos` 测试、Rustfmt、41 项迁移契约自测及普通契约扫描通过。清单 **258** 条（Rust **173**、Go **85**，含27个 HEAD 别名）、202 个审阅范围及127个 Swift HTTP 调用点。离线伪终端验证覆盖固定 AT 查询；没有实体模块查询。

Release `scripts/build.sh` 成功，`dist/Switch.app` 为 **2.86 / build 286 / arm64**，应用及嵌入的 `switchd-rs` 通过严格签名验证。只构建 `.app`，没有生成 DMG。

迁移进度按 P2A **+0.025 计划人周**计，总进度为 **12.035 / 47.5 ≈ 25.3%**（约±3个百分点）。IMS/VoWiFi、设备主要业务、N待、短信和 Go-free 验收仍未完成；路由数量不代表整体功能完成率。
