# Iteration 115 — 配置备份删除迁入 Rust

日期：2026-10-08。

将认证后的 `POST /api/call-mode/backups/delete` 从 Go 交给 Rust daemon。Rust 保留确认字段、1 MiB 请求体上限、备份 ID 的去空白/白名单校验、先读取并验证备份再检查占用状态的顺序，以及旧接口的 400/404/409/500 状态与成功 JSON。备份读取复用 no-follow 文件验证，拒绝符号链接、非普通文件、空文件和超过 64 KiB 的文件。

删除操作通过两个互补的互斥条件避免与硬件操作冲突：Rust module-radio service 的 guard 阻止并发取得 `call_mode` reservation；共享数据目录下的 `call-mode-operation.lock` 以非阻塞 `flock` 协调跨进程 Go 操作。Go 在 `call_mode` 硬件操作期间持有此 mode-0600 文件锁，Rust daemon 即使在 Go 操作中重启，删除也会因锁冲突返回原有 409。锁文件不存业务数据。Go 继续负责创建备份和执行硬件恢复。

验证结果：

- `cargo +1.98 test -p proletariusd`：**734 passed、0 failed、3 ignored**。
- `GOTOOLCHAIN=local go test -race ./cmd/proletarius-macos -count=1`、`go vet ./cmd/proletarius-macos` 通过；共享锁独占/释放/符号链接专项通过。
- `cargo +1.98 fmt --all -- --check`、`git diff --check`、契约工具 **41 项自测**、普通契约清单检查通过。
- 当前清单：**260** 条 method/path 契约（Rust **179** / Go **81**，含29个HEAD别名）、203个审阅范围、127个Swift HTTP调用点。
- Rust-only 路由门禁按预期阻断：默认 Go fallback 仍存在，148 条路由仍有 Go 所有权或依赖。该结果说明全 Rust 目标远未完成。
- `scripts/build.sh` 成功生成 **2.86 / build 286 / arm64** 的 `/Users/proletarius/Desktop/Switch/dist/Switch.app`；`codesign --verify --deep --strict` 通过。没有连接读卡器或操作实体卡，也没有生成 DMG。

本轮按工作量估计给 P2A 增加 **0.025 计划人周**，从 3.600 更新至 **3.625/5.5（65.91%）**。全项目估计为 **12.135/47.5≈25.5%（±3个百分点）**；百分比不是接口数量换算。创建备份与硬件恢复、其他 Go 硬件业务、无 Go 构建和 IMS/VoWiFi 实机验收仍未完成。呼入按钮位置修复在前一轮完成，不重复计入本轮迁移进度。

验证日志位于 `Switch/build/optimization/2026-10-08-call-mode-backup-delete-*.log`。
