# Iteration 132 — 模块语音运行时准备迁入 Rust

日期：2026-10-08。全 Rust 后端目标继续进行中；本轮只迁移模块侧运行时部署后的驱动与音频准备流程，不计设备级验收。

## Rust 接管范围

Rust 现有 `POST /api/internal/call-mode/adb/deploy-runtime` 在同一 producer generation、内部凭据、单 worker admission 与共享 `.module-adb-interface.lock` 保护下，完成本地清单工件验证、root/kernel/远端目录检查和 ADB SYNC 推送后，继续执行模块语音准备：检查旧版 `qdc507_afe` 并拒绝热切换，按需加载缺失的 `qdc507_aprv3`/`qdc507_voice`，等待目标 ALSA 节点，管理并验证 `alsaucm_test`/ACDB 校准，检查 `ttyGS0` 与 `voc_svr`，最后执行 PCM helper 自检。

原 120 秒 worker budget 扩展为包含完整部署与准备阶段的 180 秒；Go UDS 客户端为此设置 210 秒超时。超时、ADB 状态标记缺失、驱动拒绝、设备节点未出现、校准失败或 helper 自检失败均关闭本次准备流程；失败诊断保持阶段化并限长，驱动加载失败同时保留 insmod 与 dmesg 末尾信息。已经进入内核的 APR/voice 驱动不做热卸载，仍以模块重启作为安全回滚边界。

Go 的 `module_voice_darwin.go` 已移除 driver/module list、`insmod`、ALSA readiness、ACDB process/pipe 生命周期及 PCM helper `--check`。Go 仍拥有 D4/UAC route 的启动/停止、USB transition lease 及通话模式更上层状态机；它只校验 Rust 准备请求已确认后才标记本地 prepared。完整 route lifecycle、状态缓存退出和真实模块验收仍待后续迭代。

## 验证

- Rust workspace：**804 passed、0 failed、3 ignored**。新增 ADB 业务序列测试验证驱动缺失与已存在分支、legacy driver 拒绝、加载/ACDB 失败停止、日志诊断、超时预算和节点/helper 检查顺序。
- Go `cmd/proletarius-macos` 全包 `-race` 测试通过，`go vet ./cmd/proletarius-macos` 通过。
- Rustfmt、`git diff --check` 通过；迁移契约已人工复核并重建：**269 method/path（Rust 189、Go 80，含30 HEAD alias）、127 Swift HTTP 调用点、221 review scopes**；最终 `--diff` 通过。API 路径数量和归属未变化。
- 以上是离线命令轨迹/适配器证据；本轮未连接模块、读卡器或实体 SIM，未加载实际驱动、未做音频/通话测试。`scripts/build.sh` Release 构建成功，`/Users/proletarius/Desktop/Switch/dist/Switch.app` 为 2.86/build286 arm64，`codesign --verify --deep --strict` 通过；没有生成 DMG。

日志位于 `/Users/proletarius/Desktop/Switch/build/optimization/iteration132/`。

## 进度记账

P2B 增加 **+0.075 计划人周等值**，从4.275增至**4.350/8.0（54.38%）**；总体从12.745增至**12.820/47.5≈27.0%**，估算误差约±3个百分点。只计本轮运行时准备命令状态机、Go重复实现退出及离线验证；不计 Go D4/UAC route lifecycle、硬件验收或本次 UI 修正。
