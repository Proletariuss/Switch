# Iteration 121 — 通话模式备份格式校验收归 Rust

日期：2026-10-08。

Go 在新建通话模式备份前重复校验 schema、保存时间、USB 组合/还原命令和 IMS/VoLTE 字段；恢复时还重复校验已由 Rust 私有 `restore-data` owner 验证的 DTO。现移除 Go 备份格式 validator 及其重复测试，Rust `validate_import_payload` 与 `read_restorable_backup` 成为这些格式和还原资格规则的唯一 owner。备份写入继续通过带内部 token 的 Rust 私有 UDS；Rust 不可用或拒绝请求时失败关闭。

保留硬件安全边界：Go 在执行配置写入前仍读取当前模块 IMEI、USB 配置和 IMS/VoLTE 状态，并将实时 IMEI、USB VID/PID 与 Rust 返回的已验证备份比较；安全备份、通话空闲门禁、写入回读、重启与重连后的身份复核均未改动。没有执行硬件操作。

验证结果：

- Rust `call_mode_backups::tests`：**18通过**；`proletariusd` 全量：**743通过、3忽略、0失败**。新负例确认 Rust restore-data owner 拒绝被篡改的 USB 还原命令。
- Go call-mode 定向 race、`cmd/proletarius-macos` 全包 `go test -race` 和 `go vet`：通过。
- 全 Rust 迁移契约工具自测：**42通过**。在人工复核整个 Go 生产文件后更新 review 指纹并显式重写 inventory；普通全量扫描通过，清单保持 **264** 条（Rust **184** / Go **80**，含30个HEAD别名）、**210** 个审阅范围、**127** 个Swift调用点。
- `scripts/build.sh` Release 构建通过：`/Users/proletarius/Desktop/Switch/dist/Switch.app`，**2.86 / build 286 / arm64**；构建脚本执行 strict signature verification。
- 本轮没有连接读卡器、操作实体卡、发短信或呼叫，也没有生成 DMG。

此校验边界计入 P2A **+0.025计划人周**，至 **3.735/5.5（67.91%）**。整体迁移进度为 **12.295/47.5≈25.9%（估算误差约±3个百分点）**。Go 仍执行 call-mode 硬件切换、IMS/VoLTE 配置、模块重启及语音运行时管理；全 Go-free 构建/运行和设备级完整验收尚未完成。呼入按钮布局修正确认已构建，但纯 UI 工作不计入 Rust 后端迁移比例。
