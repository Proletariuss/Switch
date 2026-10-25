# Iteration 103 — Rust 短信接管 owner 与来电操作区对齐

日期：2026-10-08。继续执行[全 Rust 后端改造目标](optimization/2026-10-04-all-rust-backend-plan.md)，所有改动仅在 Switch 工作区。未操作实体读卡器或号卡，未发送短信，未生成 DMG；9eSIM `6982` 仍排除。

## 已完成

- Rust 接管 `GET/POST /api/sms/adopt` 与旧 `sms-adopt.json` 持久格式。启动时导入原偏好，运行时状态由 Rust owner 串行维护；Go 内部 USB 配置调用不再覆盖接管状态。持久化沿用兼容旧接口的 best-effort 行为，并使用原子、安全的文件替换。
- 对齐旧 Go HTTP 行为：缺省或 `null` 的 `enabled` 视为 `false`，拒绝未知字段和多个 JSON 值，限制请求体为 1 MiB，错误仍返回 HTTP 400 和既有 JSON 错误形状。测试覆盖旧文件、解码边界、并发更新、写入失败、符号链接及归档交接。
- 来电界面的接听、挂断操作组改为在通话卡片内弹性居中布局，窄窗口可收缩，不再受旧网格首列锚定而整体偏左。
- 刷新 API 契约审阅指纹与清单，保持 method/path 所有权审计可重现。

## 验证

- Rust daemon：**685 passed、0 failed、3 ignored**；`cargo fmt --check` 通过。
- Go `cmd/proletarius-macos` 全包测试通过。
- 迁移检查器：**41 项自测通过**；契约清单 **253** 条 method/path（Rust **158**、Go **95**，含 **22** 条 HEAD 派生别名）、**195** 个审阅范围、**127** 个 Swift HTTP 调用点。普通契约一致性与 `--write-inventory` 通过。
- Release `scripts/build.sh` 成功生成 `/Users/proletarius/Desktop/Switch/dist/Switch.app`；arm64 产物签名验证通过。构建保留若干既有 dead-code 与重复 libusb 链接警告。
- `git diff --check` 通过。Rust-only 门禁因默认 Go fallback 和仍由 Go 持有/依赖的路由未清零而保持失败；这表示全 Rust 目标仍在推进，不是本轮兼容迁移失败。

## 进度与边界

整体仍按既有粗估基线报告 **约 24.9%（±3 个百分点）**，本轮小范围持久化 owner 切换不足以重估工作量，也不按路由数量折算进度。完整 Go 业务退出、设备端到端验收、IMS/VoWiFi、N 待、语音与完整 LPA 迁移继续留在目标内。未生成 DMG。
