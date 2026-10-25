# Iteration 104 — 短信状态聚合迁入 Rust

日期：2026-10-08。继续执行[全 Rust 后端改造目标](optimization/2026-10-04-all-rust-backend-plan.md)，所有变更只在 Switch 工作区。未操作实体读卡器或号卡，未发送短信，未生成 DMG；9eSIM `6982` 仍排除。

## 已完成

- Rust 接管 `GET /api/sms/status`，并从 Go ServeMux 移除公开路由和旧 handler。`HEAD` 继续由 Rust 的 GET 别名处理。
- 短信数量直接计算 Rust 持久归档与 Go/USB 两个有界易失分区的合并视图；复用 GET `/api/sms` 的去重规则，不向状态响应暴露短信正文。
- 轮询状态从 Rust USB 收件 worker 读取；串口模式与轮询间隔来自 USB AT owner 的当前配置，即使首个硬件快照尚未完成也能正确显示串口模式。
- 保持既有成功响应字段 `count`、`polling`、`poll_interval_s`、`last_poll`、`last_poll_error`，以及无法确认收件箱时的 HTTP 502 / `sms_inbox_unavailable` 错误形状。
- 审计清单注明了剩余依赖：Go 仍提供非 USB/IMS 短信快照，并通过认证 USB AT 配置选择传输模式和间隔。此次是状态聚合 owner 迁移，不代表所有短信生产者或硬件链路已经脱离 Go。

## 验证

- Rust daemon：**690 passed、0 failed、3 ignored**；短信状态定向回归及归档/易失分区去重计数测试通过，`cargo fmt --check` 通过。
- Go `cmd/proletarius-macos` 全包测试通过；Go 不再注册 `/api/sms/status`。
- 迁移检查器 **41 项自测通过**，契约扫描与清单刷新通过：**254** 条 method/path（Rust **160**、Go **94**，含 **23** 条 HEAD 别名）、**196** 个审阅范围、**127** 个 Swift HTTP 调用点。
- Release `scripts/build.sh` 成功生成 `/Users/proletarius/Desktop/Switch/dist/Switch.app`；arm64 App 签名验证通过。构建仍有既有 dead-code 与重复 libusb 链接警告。
- Rust-only 门禁仍因默认 Go fallback 和 **157** 条带 Go owner/依赖的路由失败；其中状态接口自身仍依赖 Go 数据生产者与配置适配。这是当前迁移边界的显式证据，不是本轮契约回归失败。
- `git diff --check` 通过。没有实机或运营商验收，也没有生成 DMG。

## 进度与后续

全项目按既有工作包估算维持 **约 24.9%（±3 个百分点）**，不按 API 数量增加完成率。剩余 Go 短信生产者、模块/读卡器硬件业务、IMS/VoWiFi、N 待、语音及 LPA 仍属于全 Rust 目标；必须继续逐项完成生产接管与 Go 退出验收，不能把当前状态接口迁移视为短信链路已完成。
