# Iteration 327：BLE 掉线监控运行时移交 Rust

2026-10-10。变更仅在 `/Users/proletarius/Desktop/Switch`。未连接真实读卡器、未操作实体卡、未发短信/电话、未生成 DMG。

## 接管范围

- Rust daemon 以两秒节拍监视当前 Rust 权威选卡与 generation，只监控 BLE reader。
- Rust 调用现有 Go `reader/reconcile` 私有适配读取 presence；扫描错误、连接中、卡操作忙及其它不确定观察都不会累计为 absence。
- 只有连续两次不同序号的成功 absence、选卡 generation 在观察前后均一致且 Rust 原生读卡 lease 未活动时，Rust 才请求精确代次的 Go 物理清理适配。
- Go 清理适配仍重新校验 reader/generation 与 Rust reader admission，关闭成功后才停止对应 VoWiFi 并使 N 待代次失效；请求不确定时不重放卡片 I/O。
- 删除 Go 常驻 hotplug ticker、Go tracker client 与旧 Rust `open/observe` 内部 HTTP 路由。Rust gate 提供 lease 检查，锁异常时按占用处理。

## 仍未迁移

Go `reader/reconcile` 仍提供 BLE presence fact；Go 仍执行真实 BLE 物理断开和 VoWiFi/N待清理 callback。读卡器 Connect/Disconnect/Rebuild、完整 BLE/PCSC lifecycle、SIM identity 初始化及窄 pin/finalize 适配仍需迁移。没有实机拔插、SIMworld、9eSIM或运营商在线验收；9eSIM `6982` 仍排除。

## 验证

- Rust hotplug reducer：**6 passed**，包含双次确认、去重/旧代拒绝、present/indeterminate/native lease 重置及 inventory projection。
- Go adapter与现有读卡器协调定向测试通过；无硬件访问。
- `scripts/check-rust-migration-contracts.py` 通过：**340 method/path（Rust 278 / Go 62）**、127 Swift 调用点、354 fingerprinted review scopes；契约工具 **43 项**自测通过。
- `scripts/test-migration-gates.sh` 通过：daemon **1,312 passed / 3 ignored**；Go vet及全部兼容测试包通过；Swift BLE physical proof与38组 manager event sequences通过；2722 carrier cases、USB、radio、HTTP method/HEAD、codec、SDP、MESSAGE差分通过。
- 最终全量门禁日志（包含退役旧 Go ticker/tracker RPC 与旧 Rust 路由后的复跑）：`build/optimization/iteration-327-migration-gates-final.log`。Rust workspace保留两条既有 `dead_code` warning，不影响测试通过。

## 进度

该职责计入 P2B **+0.025 计划人周等值**，不重复计算 reader generation、operation admission、传输或native lease。累计 **15.370 / 47.5 ≈ 32.4%**（估算误差约±3个百分点）。构建/运行仍有7类Go依赖；整体目标未完成。
