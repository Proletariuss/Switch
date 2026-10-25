# Iteration 130 — 模块 ADB 只读探测与 USB 接口互斥

日期：2026-10-08。承接 Iteration 129；全 Rust 后端目标保持进行中。

## 本轮实现与实际归属

Rust daemon 现在承接通话模式启动中的只读 ADB root-shell 探测。探测通过内部鉴权的精确 `POST /api/internal/call-mode/adb/probe` 路由完成，并将当前 Go producer ID 交给 Rust 校验；旧 producer 的迟到请求在访问 USB 之前失败关闭。Rust 的 `--internal-adb-probe` 子模式在普通 daemon 启动之前处理，仅做隔离探测，执行固定的 `shell:id -u`，不进行文件推送、驱动安装或配置写入。

- Rust 实现有限长 ADB CNXN/OPEN/WRTE/CLSE 帧交换，限制请求、帧、输出和 12 秒总时限；认证挑战、畸形帧、非 root 响应、异常 shell 结果均返回明确失败。父进程超时会终止并回收子进程；并发许可由真实 blocking supervisor 持有到 child 退出，并额外等待 2 秒 USB 释放冷却期。
- Rust 探测与仍在使用的 Go ADB 部署路径共用 `.module-adb-interface.lock`。双方从打开设备到关闭 handle 都持有非阻塞独占 `flock`；锁文件要求为 no-follow、普通、零长度、单硬链接和 0600。这样 Rust 只读探测不会与 Go 推送/控制访问同时占用同一 USB 接口。
- Go `module_voice_darwin.go` 改为发送一次带内部令牌和 producer ID 的请求，不再自己运行 root-shell 探测。超时、owner 缺失、代次变化、超限/畸形响应及未知错误均失败关闭，不回退到 Go 的第二次 USB 探测。
- ADB CNXN/AUTH/USB 枚举、部署文件 `adb push`、驱动/helper 配置与加载、音频路由、模块重启/恢复和总体通话模式仍在 Go。该切片仅接管安全只读探测及其与 Go 使用者的 USB 互斥。

## 证据与未完成边界

| 能力 | 离线验证 | 实机验证 | 已接管 / Go 退出 |
| --- | --- | --- | --- |
| ADB root 只读探测 | 协议 transcript、分片/多帧、超时、认证请求拒绝、畸形响应、非 root 与固定输出解析由 Rust daemon 回归覆盖 | 未连接模块，不运行实际 ADB | Rust 执行探测；Go 原探测函数已移除 |
| child 生命周期与 USB 冷却 | 超时 kill/wait、worker 仍持有 admission permit、退出后 USB cooldown 有离线测试 | 未测试真实 IOUSBHost 释放时序 | Rust 私有子模式负责；真实硬件时序仍未知 |
| Go/Rust USB 互斥 | Rust 与 Go 分别验证锁文件拒绝符号链接/多链接/非空文件、占用冲突及关闭释放；Go adapter race 回归通过 | 未做真实读卡器或模块并发测试 | 两端共用同一锁；Go 仍执行 ADB 写部署 |
| 通话模式整体接管 | 当前 producer、内部鉴权、固定响应和错误映射通过离线测试 | 未测试 | Go 仍持有 push/helper、音频、重启恢复和外层状态机 |

## 验证结果

- Rust daemon 全量：**797 通过、0 失败、3 忽略**。
- Go `cmd/proletarius-macos` race 测试通过，`go vet ./cmd/proletarius-macos` 通过。
- 迁移契约工具 **42 项通过**；源码 census 一致：**269** 条 method/path（Rust owner **189**、Go owner **80**，含 30 条 HEAD alias）、**127** 个 Swift HTTP 调用点、**222** 个 fingerprinted review scope。该清单只核对接口归属基线，不代表零 Go 或后端全功能迁移。
- Rustfmt 与 `git diff --check` 通过。Debug Xcode 构建通过；`scripts/build.sh` Release 构建通过，`dist/Switch.app` deep strict 签名校验通过，版本 **2.86/build 286 arm64**。
- 证据日志位于 `Switch/build/optimization/iteration130/`。产物仍包含 Go `switchd` 与 Rust `switchd-rs`；未操作读卡器、模块、实体卡，未发送短信、未生成 DMG。

## 进度记账与下一步

本轮计入 P2B **+0.075 计划人周等值**，从 **4.100** 到 **4.175/8.0（52.19%）**；总体从 **12.570** 到 **12.645/47.5≈26.6%**，估算误差约 ±3 个百分点。只计 Rust 只读探测、child 回收与 Go/Rust USB 接口锁的实现和离线验证；未计 ADB 写部署、硬件验收或 Go 退出。

下一段继续迁移 ADB 部署与模块侧检查/恢复，将 Rust 与 Go 共享锁扩展到所有仍存留的设备访问路径，再逐步退出 Go 通话模式编排。其余硬件/LPA、IMS/VoWiFi、N待、语音和 Go-free 构建及最终设备验收都未完成。
