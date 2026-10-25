# Iteration 118 — 来电按钮居中与备份创建存储迁入 Rust

日期：2026-10-08。

来电/通话卡片中的接听与挂断按钮先前仍落在左侧窄内容列。`CallView` 的按钮组现按纵向 `ScrollView` 的实际水平宽度定位，使用 `containerRelativeFrame(.horizontal)`，使动作组位于完整来电卡片中央，而不是仅在按钮的 intrinsic content width 内居中。

通话模式配置备份现在由 Rust 持有文件创建。Go 继续采集模块与语音状态、构造既有 DTO，并通过 `PROLETARIUS_CARRIER_SOCKET` 和 `SWITCH_INTERNAL_TOKEN` 将受限 JSON 发往私有 Unix socket。Rust 对精确 `POST /api/internal/call-mode/backups/create` 执行内部 token 校验和 64 KiB body 限制，校验当前 schema、保存时间、IMEI、USB VID/PID、七项功能位、还原命令及语音字段；保留既有备份名称和 Go JSON 原始字节，使用0700目录、0600临时文件、`sync_all` 与原子改名。Rust不可用或写入失败时Go不落盘回退。Go仍负责读模块状态、执行切换和硬件还原，故整条call-mode业务尚未完成迁移。

验证结果：

- `cargo +1.98 test --locked -p proletariusd`：**741通过、0失败、3忽略**。新增UDS集成覆盖缺少token时拒绝、合法当前schema写入兼容文件名、daemon退出期间拒绝新建、非POST拒绝；备份单测覆盖 payload 字节、文件名及0600/0700权限、非法/超限输入失败关闭。
- `GOTOOLCHAIN=local go test -race ./cmd/proletarius-macos -run 'TestCallModeBackup|TestCallModeOperation' -count=1`：通过。验证Go只向Rust提交并在socket/token缺失时不创建本地备份。
- 全 Rust 契约工具：**42项通过**；全量清单扫描通过，263条方法/路径（Rust183、Go80，含30个HEAD别名）、209个审阅范围、127个Swift调用点。
- `scripts/build.sh`：Release `Switch.app` 构建及严格签名验证通过。此构建没有生成DMG。

本轮没有连接或操作读卡器/实体卡，也没有进行真实来电测试。仅编译验证了来电按钮的 SwiftUI 布局代码；实际屏幕位置还需用户运行当前 `dist/Switch.app` 确认。

P2A计入 **+0.025计划人周**：由3.660提高到3.685/5.5（67.00%）。加权总进度由12.195提高到**12.220/47.5≈25.7%**（估算误差约±3个百分点）。按钮位置修复不计入后端迁移比例。剩余Go硬件采集/调用、restore状态机、全功能零Go构建、IMS/VoWiFi实机和完整发布验收仍未完成。
