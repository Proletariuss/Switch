# Iteration 126 — 通话模式 ADB 挑战与授权迁入 Rust

日期：2026-10-08。

通话模式的旧式 QADBKEY 能力探测、8 位挑战解析、MD5-crypt 兼容口令派生和授权提交现由 Rust module-radio typed action 执行。探测只返回经过验证的能力布尔值；授权只返回已提交且确认的结果，Go 不再读取挑战、计算口令或发出敏感 AT 命令。Rust 在 CallMode reservation 内先确认通话空闲，严格要求唯一 8 位数字挑战，只提交一次；确认丢失时将结果标记不确定、退役串口传输，不自动重放。授权口令不经过 Go UDS DTO、日志或备份；Rust 清理敏感命令、终端回显和失败响应缓冲。

挑战解析增加截断 marker 拒绝，历史派生向量保持一致。通话模式用户确认、备份、USB/IMS 配置和重启恢复流程仍由 Go 编排；模块端持久授权可能无法通过配置备份撤销，这一限制继续明确展示。9eSIM `6982` 不在本轮范围。

验证：Rust workspace **767 通过、0 失败、3 忽略**；Go `go test ./...` 全包通过，`cmd/proletarius-macos` 与 `internal/modem` race 测试及 `go vet` 通过；Python 迁移/运营商脚本 **49 项通过**，迁移契约扫描器 **42 项通过**；Rustfmt、`git diff --check` 和 264 条契约扫描通过（Rust 184 / Go 80、127 个 Swift 调用点、210 个审阅范围）。Release `scripts/build.sh` 构建 Switch.app 2.86/build 286 arm64，deep strict 签名校验通过。未连接读卡器或实体卡，未执行 ADB 授权，未生成 DMG。

P2B 计 **+0.025 计划人周**，由 3.925 升至 **3.950/8.0（49.38%）**；整体由 12.395 升至 **12.420/47.5≈26.1%**（估算误差约 ±3 个百分点）。Go ADB/helper、通话模式状态机与恢复、其他硬件/IMS业务、Go-free构建和设备验收仍未完成。

验证环境记录：第一次全量 Go 测试启动时未显式给 workspace 环境设置 `SWITCH_SOURCE_ROOT`，测试的路径安全守卫因此拒绝了默认临时目录。该次运行复用了 Switch 外已有的 `/Users/proletarius/Desktop/build` 缓存并刷新了部分缓存；该目录预先存在，我没有删除或清理其中数据。随后明确将根目录设为 `/Users/proletarius/Desktop/Switch/src`，在 `/Users/proletarius/Desktop/Switch/build` 下重跑，全量测试通过。源码和正式构建产物均位于 Switch。
