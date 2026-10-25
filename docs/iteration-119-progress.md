# Iteration 119 — 通话模式备份还原读取迁入 Rust

日期：2026-10-08。

配置备份还原的两个 Go 直接文件读取点已移除。确认还原时，Go 通过带内部 token 的私有 UDS 请求 Rust 读取指定 ID；后台硬件操作开始前也重新获取同一份 Rust 校验 DTO。Rust 对精确 `POST /api/internal/call-mode/backups/restore-data` 执行身份认证、共享请求预算与停止期拒绝，限制请求体 1 KiB、校验备份 ID，并以 `O_NOFOLLOW` 打开文件，限制非空文件不超过64 KiB。Rust 校验时间、schema、USB 配置/还原命令和可还原状态；仅向 Go 返回受限结构 DTO，旧版仅可导出的备份拒绝自动还原。

Go 保留当前模块 IMEI 与 USB VID/PID 的匹配检查、操作互斥、安全备份、AT/语音配置回读、写入和重启恢复状态机。Rust 成为备份文件读取/解析/持久层 owner，但本轮不声称硬件还原状态机或整个 call-mode 功能已迁移。

验证结果：

- `cargo +1.98 test --locked -p proletariusd`：**742通过、0失败、3忽略**；新增 Rust 还原 DTO 和旧版备份拒绝测试，以及私有 UDS token、成功读取、HTTP 方法、退出期间拒绝访问的集成覆盖。
- `GOTOOLCHAIN=local go test -race ./cmd/proletarius-macos -run 'TestCallModeBackup|TestCallModeOperation' -count=1`：通过。新 Go UDS 客户端测试确认只发送 allowlisted ID、内部 token 和 Rust DTO，不创建本地备份目录。
- 全 Rust 契约工具 **42项通过**；全量清单扫描通过：**264** 条 method/path（Rust **184** / Go **80**，含 **30** 个 HEAD 别名）、**210** 个审阅范围、**127** 个 Swift HTTP 调用点。
- `cargo +1.98 fmt --all -- --check` 与 `git diff --check` 通过。
- `scripts/build.sh` Release 构建及严格签名通过，产物为 `dist/Switch.app`（2.86/build 286 arm64）。没有生成 DMG。

本轮未连接读卡器或实体号卡，也未执行真实呼叫/还原；离线回归不能证明实体模块还原结果。

P2A计入 **+0.025计划人周**：由3.685提高到**3.710/5.5（67.45%）**。加权总进度由12.220提高到**12.245/47.5≈25.8%（估算误差约±3个百分点）**。当前剩余 Go 边界包括模块状态采集、身份与业务互斥、call-mode硬件还原、IMS/VoWiFi与通话运行时，以及最终零Go构建和实机验收。
