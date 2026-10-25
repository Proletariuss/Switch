# Iteration 152：BLE 读卡器热插拔确认状态迁入 Rust

日期：2026-10-08。状态：离线代码与契约门禁已完成；未进行实体设备验收。

Rust 私有 owner 现在保存一个 hotplug monitor 的随机 token、reader identity/generation、最后 observation ID 与缺席次数。owner 拒绝旧 generation、同 generation 的 reader identity 替换、重复或倒序 observation、无效阈值和旧 token。不同 reader/generation 会开启新计数；present、indeterminate、native reader lease、确认移除和重开 monitor 都清零。缺席计数饱和到请求阈值。新动作在 daemon shutdown 后返回 `backend_stopping`，避免调用方把关闭当成读卡器移除。

Go 不再拥有热插拔缺席计数。它继续每两秒读取 BLE bridge inventory，将成功样本和当前 native lease 状态送给 Rust；inventory 错误不计作缺席。Rust RPC 超时、错误或未知响应不会断开：Go 丢弃可能已被服务端处理的 monitor token，重新打开 owner 并从新鲜观测开始。达到两次独立缺席确认后，Go 再次检查 exact BLE reader/generation、native lease 与 `authMu.TryLock`，随后断开；清理顺序仍为 stop VoWiFi，再 invalidate N-wait generation。该 Go monitor 只处理 BLE；PC/SC reader presence 不通过它扫描或断开。

## 验证

- 合并树 Rust workspace 测试1,389通过、0失败、6忽略，其中 `proletariusd` 896通过、3忽略；全量 `scripts/test-migration-gates.sh` 通过。
- 当前源码 `go test -race ./cmd/proletarius-macos -run 'ReaderHotplug|ReaderReconcile|ReaderManager' -count=1`、`go test ./cmd/proletarius-macos -count=1` 与 `go vet ./cmd/proletarius-macos`：通过。
- `cargo fmt --all -- --check`、`gofmt` 与 `git diff --check`：通过。
- `PYTHONDONTWRITEBYTECODE=1 python3 scripts/tests/test_rust_migration_contracts.py`：43 项通过。
- `PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-rust-migration-contracts.py --diff`：通过；当前清点283条 method/path（Rust211/Go72，34个HEAD alias）、127个Swift HTTP调用点、250个review scope。Rust-only门禁仍因默认Go fallback和145条Go所有权/依赖路径而失败。

本轮未重新构建 `Switch.app`，也未生成 DMG；来电按钮布局属于独立的 UI 工作，不计入本次后端迁移。未连接或操作真实读卡器/实体卡，没有发送短信、执行 APDU 或下载 LPA。Rust 只拥有 debounce/replay-fence 状态；Go 仍拥有 BLE inventory、reader session admission/final disconnect、VoWiFi cleanup 和 N-wait invalidation。


本轮进度核算：BLE 缺席确认状态有独立 Rust owner、严格私有 RPC、Rust/Go 回归和代次/租约保护，保守计入 P2B **+0.025计划人周**。累计 **13.320/47.5≈28.0%**，P2B **4.800/8.0=60.0%**。来电按钮 UI 修正不计入后端迁移比例。