# Iteration 106 — 网络服务列表迁入 Rust与呼入按钮对齐

日期：2026-10-08。继续执行[全 Rust 后端改造目标](optimization/2026-10-04-all-rust-backend-plan.md)。工作仅在 Switch 工作区内完成；未改网络服务顺序、未操作读卡器或号卡、未发送短信、未生成 DMG。9eSIM `6982` 仍排除。

## 已完成

- Rust 接管 `GET /api/network/services`：在本机运行 `networksetup -listnetworkserviceorder`，解析服务名、设备、端口、禁用状态与 USB 标记，并返回原有 `services` JSON 投影；Go 删除了该 GET handler。Go 的 `PUT /api/network/services-order` 仍负责顺序变更。
- 为保留旧 `module` 标记，Rust 在枚举前向 Go `GET /api/health` 读取 USB 产品名。因此该接口的枚举与解析归 Rust，仍有一个明确的 Go 状态依赖；不把它标为 Go-free。
- Swift 通话页的呼入按钮行移除了导致左偏的固定外层宽度布局。接听/挂断按钮作为固定宽度组放在弹性容器中央，按通话卡片可用宽度居中。

## 验证

- Rust `proletariusd` 全量：**698 passed、0 failed、3 ignored**。
- Go `go test ./cmd/proletarius-macos -count=1` 通过。
- 迁移契约扫描通过：**256** 条 method/path（Rust **166**、Go **90**，含 **25** 条 HEAD 派生别名）、**199** 个人工审阅范围；扫描报告 **127** 个 Swift HTTP 调用点。Rust-only 门禁仍因 Go fallback 与未迁移依赖而不通过。
- 契约工具自测：**41 项通过**。HEAD 只继承 GET 的所有权与依赖，不另算迁移收益。
- `scripts/build.sh` Release 构建成功；`dist/Switch.app` 为 **2.86 / build 286**、arm64，严格签名验证通过。构建保留已有 macOS API 弃用、Rust dead-code 和重复 `libusb` 链接警告。
- `cargo fmt --all -- --check` 与 `git diff --check` 通过；Rustfmt 使用 Switch 锁定的 1.98 toolchain，未安装全局组件。

## 进度与后续

网络服务枚举/解析是管理 API 的局部接管，按 **0.04 计划人周**计入 P2A，完成度从 **61% 调整到约 61.73%**；原 **47.5 人周**基线为 **11.905 / 47.5 ≈ 25.1%**（误差仍约 ±3 个百分点）。路由计数不等价于业务或全项目完成率。Go health 状态依赖、网络顺序变更、设备业务、IMS/VoWiFi、N 待、短信与语音生产路径以及最终 Go-free 验收仍未完成。呼入按钮位置修复不计入 Rust 迁移比例。
