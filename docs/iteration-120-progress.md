# Iteration 120 — 移除 Go 备份文件读取旁路

日期：2026-10-08。

Rust 已接管配置备份公开列表、导出、导入、删除、创建和恢复读取后，Go 源码仍遗留一套未注册的备份列表 handler、直接文件 reader、DTO decoder 与摘要投影。该旁路没有生产路由调用，却保留了第二份文件格式/存储逻辑。本轮删除这套 Go 实现及其旧文件系统测试，保留 Go 对备份 DTO 的硬件安全校验、Rust UDS 创建/读取客户端，以及真实 call-mode 硬件恢复状态机。Rust 的边界测试继续负责文件权限、符号链接、列表排序、旧schema与恢复资格；没有因此改变公开接口或设备行为。

验证结果：

- Go `cmd/proletarius-macos` 全包 `go test -race`：通过；`go vet`：通过。备份 DTO、Rust UDS 调用与 call-mode operation 定向 race 回归通过。
- Rust call-mode backup 专项：**17项通过**，覆盖 Rust owner 的创建、列表、导入、导出、删除、恢复读取和 legacy/schema 语义。
- 全 Rust 契约工具 **42项通过**；全量扫描保持 **264** 条 method/path（Rust **184** / Go **80**，含 **30** 个 HEAD 别名）、**210** 个审阅范围、**127** 个 Swift HTTP 调用点。公开路由没有变化。
- Rustfmt、`git diff --check` 和 Release `scripts/build.sh` 通过；签名后的 `dist/Switch.app` 为2.86/build 286 arm64。未生成 DMG。
- `rg` 确认已删除的 Go reader/list/summary/parser 符号没有残留于 Go backend。

本轮是迁移收口清理，不等于 call-mode 还原业务已迁入 Rust。Go 仍执行模块身份读取、USB/IMS/VoLTE硬件状态机、AT回读、模块重启与语音运行时管理；无实体卡或真实通话测试。

计划工作量累计增加 **0.025人周**，记入 P6 的 Go 退出准备；总进度从12.245提高到**12.270/47.5≈25.8%（估算误差约±3个百分点）**。本次未改变 P2A 的 **67.45%**；总进度显示到一位小数未变化。零 Go 构建/运行/嵌入依赖、模块硬件业务、IMS/VoWiFi/语音和实机验收仍待完成。
