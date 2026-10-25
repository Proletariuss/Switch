# Iteration 138：模块通话接口就绪判断迁入 Rust

日期：2026-10-08  
工作区：`/Users/proletarius/Desktop/Switch/src`

## 改动

Rust 私有 call-mode 状态投影新增 `usb_interfaces` 与 `call_interfaces` 两个无 I/O 阶段。前者校验 typed USB composition 并判定 ADB/UAC 是否齐备；后者校验 USB 与 voice tuple，并由 Rust 计算 ADB + UAC + IMS/VoLTE 的最终 `interfaces_ready`。两个阶段都拒绝无效数据及混入的运行时下载、ADB 探测等无关事实。

Go 的 `callModeInterfacesReady()` 已移除本地 `hasADB()`/`hasUAC()` 和语音 ready 值的组合判断：先读 USB，并把 ADB/UAC 判断交给 Rust；仅 USB 阶段返回 ready 时才继续读取 IMS/VoLTE；最终组合结果再由 Rust 返回。原有 USB 未就绪时提前返回、IMS/VoLTE 未就绪错误文案、探测 ADB 的先后顺序均保留。Rust 投影不可用、输出状态不一致或事实无效时失败关闭。

本轮新增的是已有认证私有路由的阶段，不新增公开 HTTP 路由。通话模式高层状态机、USB/语音事实采集、transition lease、ADB probe、硬件写入、重启与恢复仍由 Go 编排。

主要代码范围：

- Rust：`rust/crates/proletariusd/src/call_mode_status.rs`
- Go：`backend/cmd/proletarius-macos/call_mode.go`、`call_mode_status_projection.go`、`call_mode_status_projection_test.go`
- 契约审阅：`docs/optimization/all-rust/api-contract-review.json` 与生成清单 `api-contract-inventory.json`

## 离线验证

- `cargo +1.98.0 test --workspace --offline --locked`：**822 通过、0 失败、3 忽略**。
- 通话模式 Rust 投影专项测试：7 项通过；Go 新增投影测试通过。
- `go test -race -count=1 ./cmd/proletarius-macos` 与 `go vet ./cmd/proletarius-macos`：通过。
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts.tests.test_rust_migration_contracts`：42 项通过。
- `python3 scripts/check-rust-migration-contracts.py`：通过；276 个 method/path（Rust 195、Go 81，含 30 个 HEAD alias）、127 个 Swift HTTP 调用点、224 个审阅范围。
- `cargo +1.98.0 fmt --all -- --check` 与 `git diff --check`：通过。
- Rust-only 路由门禁仍失败：默认 Go fallback 存在，150 条路由仍有 Go owner/dependency。

## 构建及边界

`scripts/build.sh` 构建 Release `Switch.app` **2.90 / build 290 arm64**，`codesign --verify --deep --strict` 通过。应用位于 `/Users/proletarius/Desktop/Switch/dist/Switch.app`，体积 83 MB；内嵌 `switchd-rs` SHA-256：`20906ad4441bd2825b60afb91a10b88db8274fb0c4f6b37afbb57f31f3dab5cc`。包中仍同时包含 Go `switchd` 和 Rust `switchd-rs`，因此本轮构建不代表零 Go。

没有连接或操作读卡器、模块或实体 SIM，没有发送短信或发起呼叫，也没有生成 DMG。没有实机业务或长期稳定性验收。9eSIM `6982` 仍排除。

## 进度

保守计入 P2B **+0.025 计划人周等值**：4.525 → **4.550 / 8.0（56.88%）**。全项目按既定 47.5 人周基线为 12.995 → **13.020 / 47.5（27.4%）**，估算误差约 ±3 个百分点。

Go 仍保留通话模式总体状态机、公开状态/cache、事实采集、ADB 探测编排、USB transition lease、硬件变更及重启恢复。其他硬件、LPA、IMS/SMS/N待、转发及完整语音仍有 Go owner；Go-free 构建、实体设备验收、数据升级/回退和长稳验收均未完成。本轮不计 UI 修改或硬件验收。
