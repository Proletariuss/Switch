# Iteration 438：激活 Rust VoWiFi 会话清理 supervisor

日期：2026-10-13

## 结果

将 Rust keyed-session lifecycle 中的受控恢复 worker 接入 `switchd-rs` daemon 启动与现有有序关闭流程。daemon 现在持有该 service owner 的完整进程生命周期；关闭时先停止准入，再等待恢复 worker 退出。服务启动本身不会打开读卡器、启动 IKE/IMS、声称 VoWiFi 可用或启用会话准入；真实 VoWiFi 入口仍被 SMS/call runtime 与 owner readiness gate 阻止。

这推进了 P1 的运行底座，但没有迁移 Go 业务 owner、路由或构建依赖，因此不增加加权迁移进度：仍为 **37.99%（18.045/47.5，估算误差约 ±3 个百分点）**。Rust-only 退出检查仍有 6 类阻塞、266 个生产 Go 源文件。

## 验证

- Rust formatting 检查通过。
- `proletariusd` 全量离线测试：**1445 passed / 0 failed / 3 ignored**。
- `proletariusd` 生产二进制构建通过。
- `scripts/test-migration-gates.sh`：完整迁移门禁通过，包含 Rust workspace、Go tests/vet、Swift BLE 回归、2722 项 carrier replay 与既有 wire/projection parity。
- 契约清单已刷新并通过 `scripts/check-rust-migration-contracts.py --diff`：354 routes（Rust 293 / Go 61）、127 Swift 调用点、378 审阅范围。
- `scripts/check-rust-backend-exit.py` 仍如实报告 6 类阻塞；本轮未减少任何构建/运行/源码依赖。
- 新增 lifecycle 测试验证 supervisor worker 可启动并有序排空，以及 shutdown admission 关闭后不能重新安装。
- 未接触读卡器、实体卡、运营商网络；未验证实机 VoWiFi 会话、SMS 或通话。
- 此轮没有生成 DMG；现有 3.15/build315 DMG 不包含本轮 lifecycle 启动接线。

## 后续边界

下一步需把 Rust keyed actor 接入单 Profile 的真实 IMS transport 与受保护 SMS/call 命令分派，再逐项开放 readiness gate。不能仅因后台 recovery worker 已启动就切换 `/api/reader/vowifi` 或宣称 P3 已接管。
