# Iteration 137：IMS/VoLTE 就绪判定迁入 Rust

日期：2026-10-08  
工作区：`/Users/proletarius/Desktop/Switch/src`

## 改动

Rust 私有通话模式投影新增 `voice_facts` 阶段，Rust 现在独立判定 IMS/VoLTE 是否就绪。该阶段只接受通过 schema 校验的 IMS、VoLTE capability 和 VoLTE disabled 三元组，并拒绝 USB、ADB 探测、运行时下载进度或错误等无关事实；它不执行设备或文件 I/O。有效但未就绪的值映射为 `needs_voice`，就绪值映射为 `ready`。

Go 的 `callModeVoiceConfiguration.ready()` 已移除。来电准备、模块重启后的 IMS/VoLTE 确认以及备份还原状态现在调用已认证、限长的 Rust 私有投影。无法访问 Rust、响应无效或事实不一致时保持失败关闭；还原过程在写配置前完成当前状态与备份目标状态投影。Go 仍负责高层通话模式状态机、公开状态/cache、设备事实采集、USB transition lease、硬件操作及恢复流程，本轮不是完整 call-mode 迁移。

主要代码范围：

- Rust：`rust/crates/proletariusd/src/call_mode_status.rs`
- Go：`backend/cmd/proletarius-macos/call_mode.go`、`call_mode_backup.go`、`call_mode_status_projection.go`
- 契约审阅：`docs/optimization/all-rust/api-contract-review.json` 与生成清单 `api-contract-inventory.json`

## 离线验证

- `cargo +1.98.0 test --workspace --offline --locked`：**820 通过、0 失败、3 忽略**。
- `go test -race ./cmd/proletarius-macos` 与 `go vet ./cmd/proletarius-macos`：通过。
- `python3 -m unittest scripts.tests.test_rust_migration_contracts`：42 项通过。
- `python3 scripts/check-rust-migration-contracts.py`：通过；276 个 method/path（Rust 195、Go 81，含 30 个 HEAD alias）、127 个 Swift HTTP 调用点、224 个审阅范围。
- `cargo +1.98.0 fmt --all -- --check` 与 `git diff --check`：通过。
- Rust-only 路由门禁仍失败：默认 Go fallback 存在，150 条路由仍有 Go owner/dependency。该结果反映未完成范围，不作为本轮失败。

## 构建及边界

`scripts/build.sh` 构建 Release `Switch.app` **2.89 / build 289 arm64**，`codesign --verify --deep --strict` 通过。应用位于 `/Users/proletarius/Desktop/Switch/dist/Switch.app`，体积 83 MB；内嵌 `switchd-rs` SHA-256：`139cdfa00d9db237e3b991239696a31383a63d877c554ed83dd044609ff9e7fc`。构建仍包含 Go `switchd` 与 Rust `switchd-rs`，因此只证明迭代集成可构建，不能证明 Go-free。

没有连接或操作读卡器、模块或实体 SIM，没有发送短信或发起呼叫，也没有生成 DMG。未验证运营商业务、硬件行为和长期稳定性。9eSIM `6982` 仍排除。

## 进度

保守计入 P2B **+0.025 计划人周等值**：4.500 → **4.525 / 8.0（56.56%）**。全项目按既定 47.5 人周基线为 12.970 → **12.995 / 47.5（27.4%）**，估算误差约 ±3 个百分点。

仍待迁移的 call-mode 工作包括 Go 高层状态机、公开状态/cache、ADB 与设备观察编排、USB transition lease 和重启/恢复流程。其它硬件、LPA、IMS/SMS/N待、转发及完整语音仍有 Go owner；Go-free 构建、实体设备验收、数据升级/回退和长稳验收均未完成。本轮不计呼入按钮 UI 修正或任何硬件验收。
