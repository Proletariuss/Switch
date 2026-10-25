# Iteration 179：旧 eSIM operation 查询别名接入 Rust

日期：2026-10-09。所有变更和验证产物均位于 `/Users/proletarius/Desktop/Switch/`。

公开 `GET /api/esim/operation` 现在由 Rust 直接分派到已有 `reader_esim_download::get_operation`，与 `/api/reader/esim/operation` 共用同一状态合并逻辑及认证 Go 上游读取。Go 原处理器仍保留给显式独立兼容模式。此次只接管旧查询别名；Go 提供的上游操作事实仍是依赖，所以不增加计划人周进度。

Rust GET/HEAD method-policy、owner 差分和完整迁移门禁通过：`proletariusd` 1,011 项通过、3 项忽略；Go 全仓测试与 `vet`、2,722 项载波回放、USB/radio/HTTP-HEAD/codec-SDP parity 通过。契约清单为 311 条（Rust 245、Go 66，含 42 个 HEAD alias）、127 个 Swift 调用点、287 个审阅范围。未进行设备测试、App 打包或 DMG 构建。

总体进度维持 **14.145/47.5（29.8%）**。全 Rust 后端、Go-free 构建/运行和实机验收仍未完成。
