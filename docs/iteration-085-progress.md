# Iteration 085：Rust runtime readiness 与 Celmux 组合根复核

日期：2026-10-05。所有 Switch 源码改动位于 `/Users/proletarius/Desktop/Switch/src`；Celmux 参考 `/Users/proletarius/Downloads/celmux-src/` 只读。本轮不生成 DMG，也不访问真实读卡器或蜂窝网络。

## Celmux 复用判断

重新审阅 `crates/runtime/src/lib.rs`：它用 Control、IMS、Media 三条显式调度 lane 隔离不同生命周期，以唤醒/截止时间推进 `RuntimeHost`，并由 scheduler 收停和 join。文件 SHA-256：`fca3a73d63e43765ba320e8668fec2df05dd9e14d45fd3039b06fe8ed5c4ba22`；crate manifest 声明 PolyForm Noncommercial。

Switch 当前已用 Tokio daemon、`BackendAvailability` 和专属设备 actor 管理任务。直接搬入第二套 Celmux scheduler 会重复运行时机制，并新增许可边界；因此本轮只借鉴“每个生命周期状态有明确 owner、网关 readiness 与硬件 readiness 分开呈现”的结构，不复制 Celmux 源码。此前已进入 Switch 的 MIT 协议层复用继续按 [Celmux 复用评估](optimization/2026-10-04-celmux-reuse-review.md) 中的文件级许可与设备 owner 边界执行。

## 改动

Swift `BackendReadiness` 过去通过 Go-owned `/api/health` 决定 Rust gateway 进程是否启动成功。现在 Rust gateway 模式检查 Rust-owned `/api/v2/runtime` 的认证响应和 `ok` 字段；Go 的 `go_backend_connected` / phase 仍单独呈现，现有 `/api/health` 继续服务设备快照和业务功能。显式 `go-compatibility` 紧急回退仍检查 `/api/health`，避免改变回退路径。

这让应用能区分“Rust API 网关已经提供服务”和“Go 硬件业务仍在启动/重启”。它没有更改 Go 硬件路由、也没有移除 Go 的构建或运行依赖；UI 首次显示后，依赖 Go 的功能仍按各自接口状态工作。

为让 macOS Unix socket 回归夹具可以使用短路径，`test-backend-readiness.sh` 增加可选 `SWITCH_TEST_TMPDIR`，默认仍用工作区隔离的 `TMPDIR`。本轮通过该变量将所有夹具放到 Switch 的 `build` 目录。

## 验证

- `SWITCH_TEST_TMPDIR=/Users/proletarius/Desktop/Switch/build/t /Users/proletarius/Desktop/Switch/src/scripts/test-backend-readiness.sh`：通过。覆盖认证、重复启动、Go 尚未连接时 Rust readiness、Go compatibility health 回退、异常退出、畸形/错误响应、5秒期限、取消隔离、线程/FD 稳定性及旧进程回调栅栏。
- Swift 测试打印：`PASS Rust gateway readiness is independent of Go hardware startup`、`PASS Go compatibility mode retains /api/health readiness`；100 组取消/健康请求后线程与 FD 无增长。
- `git diff --check`：通过。
- Rust migration route census：在人工审查两个按模式选择的 readiness literal 后，用 `--write-inventory` 更新审计快照，再以默认只读模式验证通过；仍为 196 method/path、Rust owner 93、Go owner 103、Swift HTTP 调用点 127。
- 首轮使用较深的测试目录触发 socket path-too-long，且工作区环境脚本重置 `TMPDIR`；已在测试脚本加独立目录覆盖项并以 Switch/build 短路径重跑通过。没有放宽产品断言。
- Rust daemon 本轮无源码更改；不重复运行上一轮已通过的 crate 全量测试。

## 进度

P0/P1 运行底座估计从 42% 调整为 43%（+0.04 计划人周），反映 Swift→Rust gateway readiness 已实现并离线验证；队列容量、物理设备 actor、完整关停排空与实机硬件 ready 仍待完成。固定 47.5 人周基线下，总进度为 **10.507 / 47.5 = 22.1% 已完成，77.9% 待完成**，估算误差约 ±3 个百分点。当前仍处于 P2B 硬件/LPA 阶段，Celmux MIT SIP 协议层尚未连接生产 IMS owner，Go 构建和运行依赖仍在。
