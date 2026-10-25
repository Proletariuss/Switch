# Iteration 109：网络服务顺序设置迁入 Rust

日期：2026-10-08（Asia/Shanghai）  
范围：持续推进 Switch 全 Rust 后端迁移；只在 `/Users/proletarius/Desktop/Switch/src` 工作，不生成 DMG。

## 结果

Rust daemon 接管 `PUT /api/network/services-order`。请求沿用至少两个服务名、trim、当前系统服务清单校验和拒绝重复项等既有语义。系统服务查询使用有界 `networksetup -listnetworkserviceorder`；有效变更由管理员授权的 `osascript` 执行。生成的 shell 命令对服务名作 POSIX 单引号转义，再以 Base64 传入 AppleScript。命令期限为 3 分钟，输出分别限制为 1 MiB。

Go 删除此路由和对应验证/脚本生成实现。Rust 路由位于 Go recovery admission 之前。网络服务列表与流量读取仍依赖 Go health 中的 USB 产品信息，完整 `/api/network` AT 诊断也仍由 Go 提供，所以这一迁移不代表网络管理已完全脱离 Go。

## 验证

- `cargo +1.98 test --offline -p proletariusd`：712 通过、3 忽略、0 失败。
- `GOTOOLCHAIN=local go test ./cmd/proletarius-macos -count=1`：通过。
- `cargo +1.98 fmt --all -- --check`：通过。
- 迁移契约扫描：257 条 method/path（Rust 170、Go 87，含 26 个 HEAD 别名），201 个审阅范围、127 个 Swift HTTP 调用点；普通一致性扫描通过。
- 契约工具自测：41 项通过。Rust-only 门禁仍因默认 Go fallback 和剩余 Go owner/依赖路由而阻断，符合当前未完成状态。
- 离线测试验证 JSON、名称检查与转义脚本；没有调用 `osascript` 或改动真实网络服务顺序。

## 进度与边界

本项计入 P2A 计划工作量 +0.040 人周；总进度约为 12.010/47.5 人周（25.3%，估算误差约 ±3 个百分点）。该比例是工作包加权估算，不以迁移路由数量代替功能完成率。

尚未完成的范围包括完整网络诊断及 Go health 身份依赖、读卡器和设备业务、IMS/VoWiFi、N 待、语音以及移除 Go 构建/运行依赖的最终验收。没有操作实体号卡或发送短信，也未生成 DMG。`scripts/build.sh` 已完成 Release 构建；产物为 2.86/build 286 arm64 `Switch.app`，`codesign --verify --deep --strict` 通过。
