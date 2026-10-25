# Iteration 303：Rust 通话状态适配器边界校验

Rust 公开通话控制路由此前会把 Go 状态适配器的 JSON 状态直接用于 UI 与拨号/接听/DTMF 准入。本轮在 Rust 侧收紧该边界：只接受已知状态（`idle`、`incoming`、`dialing`、`alerting`、`active`、`unknown`）、必需布尔字段，以及长度受限的可选号码和历史错误。结构无效时，状态查询以兼容的 `unknown` 快照响应，控制准入 fail-closed，避免异常 JSON 状态授权控制动作；现有 Go/硬件操作路径和响应错误语义保持不变。

验证：通话控制专项 **19 passed**；Rust workspace **1192 passed / 3 ignored**；`scripts/test-migration-gates.sh` 全部通过，含 Go vet/兼容测试、Rust/Go parity 与 HTTP method/HEAD 检查。门禁需要 Go 1.26.3 时自动下载超时，本轮通过 Switch/build 内的临时 Go shim 使用已安装 Go 1.26.7 完成校验，shim 已清理。`cargo fmt --check` 与 `git diff --check` 通过。

该工作加固了 Rust 已拥有的 public policy boundary，但实际通话状态采集、URC/CLCC 协调、呼叫生命周期、SIM/USB 命令与 IMS media 仍有 Go owner；没有迁移生产硬件或媒体 owner，因此不增加加权迁移进度，仍为 **31.4%**。未做实机通话、短信、读卡器操作或 DMG 构建。
