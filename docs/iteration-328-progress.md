# Iteration 328：Rust 直接拥有 BLE presence 观察

2026-10-10。所有代码和验证产物只在 `/Users/proletarius/Desktop/Switch`。未访问读卡器、未操作实体卡、未收发短信/通话、未生成 DMG。

## 生产职责移交

- Rust reader inventory 和 BLE 掉线监控直接读取已认证的 Swift CoreBluetooth 状态桥，不再经过 Go `POST /api/internal/reader/reconcile` 获取连接身份。
- 状态策略保留明确边界：精确连接的 BLE 身份为 present；另一身份连接时，当前选中身份为 absent；连接/发现/重连中、桥接超时或桥接不可用均为 indeterminate。未知或自相矛盾状态失败关闭，不作为掉卡证据。
- 热插拔观察前，Rust gate 检查该 reader 的实体租约、业务预约、worker、BLE I/O 和 operation lease。存在活动时跳过桥探测并将本轮作为 indeterminate，避免串行 BLE 队列中的探测超时被累计为 absence。
- Rust 仍以两次不同序号且同一 reader generation 的 absence 才发起清理。Go 只保留已授权的精确代次物理断开、停止对应 VoWiFi 和失效 N 待代次适配。
- 移除 Go reconcile 私有路由；契约从 340 项降至 **339 项（Rust 278 / Go 61）**。Rust 清单扫描器不把路由数量当迁移完成率。

## 验证

- Rust BLE bridge status：**12 passed**；覆盖精确连接、其他 reader、过渡态、不一致/未知状态、超长帧及不可用桥接。
- Rust hotplug reducer：**5 passed**；reader gate 工作中保护测试：**1 passed**。
- 全量 `scripts/test-migration-gates.sh`：daemon **1,316 passed / 3 ignored**，Go vet/测试、Swift BLE 物理回执与 38 组 manager 序列、2,722 carrier cases、USB/radio/HTTP/codec/SDP/MESSAGE 差分通过。
- 契约清单检查通过：**339 method/path（Rust 278 / Go 61）**、127 Swift HTTP 调用点、354 审阅范围；契约扫描器 **43 项自测通过**；`git diff --check` 通过。
- 完整门禁日志：`build/optimization/iteration-328-migration-gates.log`。日志中的两条 Rust `dead_code` 警告与既有状态相同。

## 仍未完成

Go 仍执行物理 reader disconnect 与 VoWiFi/N 待 cleanup，保留 reader connect/rebuild、身份获取/初始化和 pin/finalize 适配；读卡器全生命周期尚未移交。没有做硬件拔插和运营商验收，因此本轮只证明离线等价路径，不代表真机掉线恢复通过。9eSIM `6982` 继续排除。

本次独立计入 P2B **+0.025 计划人周等值**，不与 Iteration 327 的调度/absence 策略重复计算。累计 **15.395 / 47.5 = 32.41%（约 32.4%，估算误差约 ±3 个百分点）**。本轮增量约 **0.05 个百分点**。
