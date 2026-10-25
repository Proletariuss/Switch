# Iteration 433：通话模式重试准入迁入 Rust

日期：2026-10-13。全 Rust 后端目标继续进行中。本轮只迁移重试请求的公共入口与是否重试决策，硬件复位和语音运行时执行仍由 Go 兼容层完成。

## 实际变更与职责边界

- Rust gateway 接管 `POST /api/call-mode/retry`，执行方法、请求体大小、状态结构和是否可重试的校验；不可重试时返回当前快照，不触发硬件动作。
- Go 移除该公开路由，保留经过内部认证的 observe/start 适配器。Start 再次读取硬件状态并重新验证后，才沿用原操作锁、异步复位和模块运行时准备路径。
- 该切片不代表通话模式硬件操作或模块语音运行时已完全迁入 Rust；Go 仍是硬件和部分运行时执行依赖。

## 验证与边界

- `scripts/test-migration-gates.sh` 全量门禁通过，日志 SHA-256：`ec8af6b2a1afeb4287cf778fa7262d21802bebc675c9cdfa7a933a2d1e696326`。
- Rust UDS/HTTP 回归验证不可重试路径只执行 observe、可重试路径依次执行 observe/start；Go 适配器认证和严格空请求体测试通过。API 契约 diff 与 `git diff --check` 通过。
- 契约清单：351 method/path（Rust 290 / Go 61）、127 Swift 调用点、375 reviewed scopes。Go exit audit 仍有 6 类阻塞、266 个生产 Go 源文件。
- 未做真实模块复位、语音运行时下载/部署、读卡器或运营商实机测试。

## 进度

本轮计入 P2B +0.025 计划人周等值；累计 **17.970 / 47.5 = 37.83%**（误差约 ±3 个百分点）。该比例是按迁移范围和验证证据核算，不代表 Rust 后端已完成。

本轮后构建并验证了 **3.13 / build 313** DMG，发行记录见 [release-3.13](release-3.13.md)。
