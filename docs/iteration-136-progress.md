# Iteration 136：模块语音运行时代次分配权迁入 Rust

日期：2026-10-08  
工作区：`/Users/proletarius/Desktop/Switch/src`

## 改动

Rust daemon 现在拥有模块语音运行时代次的分配权。新增认证的内部 POST 动作；它只接受当前 Go producer 标识，由 Rust 原子递增代次并清除旧的准备缓存。代次耗尽、旧 producer、过期令牌及尚未由 Rust 签发的前进令牌均失败关闭。Go 不再自行递增代次，只缓存并转交 Rust 签发的令牌；重置本地缓存后，后续状态查询或部署会先向 Rust 申请新令牌。

Rust 继续将准备状态绑定到 producer、运行时代次和模块 boot ID。该轮没有扩大设备操作范围：运行时部署与 D4/UAC 路由仍由 Rust 执行；Go 仍负责通话模式总状态机、公开状态 DTO/cache、USB transition lease 与重启恢复。此轮不等同于完整 call-mode owner 退出。

涉及 Rust `call_mode_adb.rs`、daemon 路由与 ownership 声明，以及 Go 的认证 UDS 客户端和 module voice 适配器。接口只在私有 UDS 上可用，不新增公开 Swift HTTP 路由。

## 验证

- `cargo +1.98.0 test --workspace --offline --locked`：819 通过、0 失败、3 忽略。
- `cargo +1.98.0 fmt --all -- --check`、`git diff --check`：通过。
- `go test -race ./cmd/proletarius-macos` 与 `go vet ./cmd/proletarius-macos`：通过。
- `PYTHONDONTWRITEBYTECODE=1 python3 scripts/tests/test_rust_migration_contracts.py`：42 项通过；源码清单一致性扫描通过，276 条 method/path（Rust 195、Go 81，含 30 条 HEAD alias）、127 个 Swift HTTP 调用点、222 个审阅范围。
- `scripts/build.sh`：Release `Switch.app` 2.88 / build 288 arm64 构建成功，`codesign --verify --deep --strict` 通过。内嵌 `switchd-rs` SHA-256：`1bf099d24d528dffe6813475eb3769c68d2fc2fa2a10c88cff34141751677748`。

没有连接读卡器或模块，没有变更实体卡、发短信或拨打电话；没有生成 DMG。UI 来电按钮居中修正也随 2.88 应用构建验证，但不计入后端迁移进度。

## 进度与未完成项

保守计入 P2B **+0.025 计划人周等值**：4.475 → **4.500 / 8.0（56.25%）**。全项目按既定 47.5 人周基线为 12.945 → **12.970 / 47.5（27.3%）**，误差约 ±3 个百分点。

Go 仍保留通话模式总体状态机、公开状态缓存、事实采集与 ADB 探测编排、USB transition lease 及重启/恢复流程；其他模块、LPA、VoWiFi/IMS、N待、短信和完整语音能力也未完成 Go 退出。Rust-only 门禁、无 Go 干净构建、实机功能、数据升级/回退与长期稳定性验收均未通过或尚未进行。不得将本轮视为全 Rust 完成。
