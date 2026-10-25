# Iteration 325：Rust 接管读卡器原生租约生命周期

2026-10-10。生产接线与离线验证已完成；未操作实体卡、发送短信、拨号或生成 DMG。已交付的 2.97 DMG 冻结至 Iteration 322，本轮源码不在该安装包中。

## 生产职责变化

Rust 在首个 RPC 前生成租约身份与递增序号，原子绑定 Go 进程、选中读卡器、身份代次及操作策略。Rust 协调器持有创建、续期、到期、取消、切卡收尾和释放状态；HTTP 等待者取消不能终止已经承担的清理责任。实际卡片 worker 在阻塞工作结束前保留占用。

Go 原有原生租约 token、TTL/watchdog、续租与清理协调器退役，旧 `reader/session-lease` 路由删除。Go 保留六条窄适配接口：`pin/status/unpin/finalize/finalize-status/finalize-retire`，负责固定现存 Go 消费者、返回绑定事实和执行已有复位/刷新回调，不再自行分配或计时撤销租约。

全部原生 LPA、公开和租约内 AKA、原生 VoWiFi 启动消费者改用该协调器。PC/SC 续期与释放由 Rust 直接调用同一物理 owner；BLE 与 PC/SC 的物理完成证明独立于 RPC 是否返回。

## 异常路径与身份一致性

- 首个 pin 响应丢失后只查询或释放原绑定；取消先到的墓碑阻止迟到请求重新占卡。身份事实损坏与物理绑定恢复分开处理，不能丢失释放所需的凭据。
- Go 退出后仍等待实际 PC/SC worker 退出和句柄关闭，以及 BLE 工作结束；进程退出、逻辑超时都不代替物理证明。
- 切卡收尾只提交一次。状态未知时退休原请求，已开始的回调继续完成，未开始的回调留下不可逆的取消回执。不会再次发送切卡 APDU 或重复复位。
- 最终回调结束后若选择转换悬挂，或原 reader/generation 根本没有更新，清空旧选择并推进代次，要求重新读取身份。已经提交的新代次即使回复丢失也保留。
- Go unpin 同步 Rust 的最终选择并清除旧身份缓存；读取确认失败时隔离旧身份、保持占用，不能将旧 ICCID/IMSI 交给后续请求。仅在回调成功、目标 ICCID 已确认、无悬挂或不确定转换，且 Go/Rust 精确匹配高于原绑定的新 reader/generation 时保留新缓存，避免破坏成功切卡后的后继租约。

交叉审查发现并修复了“EnableProfile 已受理、finalize 尚未入账”仍保留旧身份的问题。取消、确认回执与新身份投影现在分别验证，不能用 `terminal=true` 单独证明切卡后的身份已经更新。

## 验证记录

- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,823 passed / 6 ignored**，daemon **1,294 passed / 3 ignored**；Go vet/全包测试、Swift 实际 BLE Unix 桥/操作账本和 38 组 manager 事件序列、carrier/USB/radio/HTTP/codec/SDP/MESSAGE 差分均通过。
- 完整门禁之后的最终 Go 缓存保留修正再次通过 **14 项 pin 竞态测试**、`go vet ./...`、`go test ./... -count=1`（26 个测试包通过，4 个包无测试）。此后没有新的生产修改，不重复运行不受该修正影响的 Rust/Swift 测试。
- 契约刷新与核验通过：**338 method/path（Rust 277 / Go 61）**、127 个 Swift 调用点、349 个审阅范围；扫描器 43 项自测通过。Go pin 新增取消回执接口，不据接口数量增加迁移进度。
- 零 Go 结构检查仍报告 **7 类构建/运行/源码依赖**，258 个生产 Go 文件；按预期未通过最终退出要求。没有新 App 构建/安装，不能把离线通过解释为实机通过。

最终日志均位于 `Switch/build/optimization/`：完整门禁和退出审计使用 `2026-10-10-iteration-325-migration-gates.log`、`2026-10-10-iteration-325-go-exit-audit.log`；最后 Go 复验及契约使用 `iteration-325-go-pin-cache-final-race.log`、`iteration-325-go-cache-final-vet.log`、`iteration-325-go-cache-final-all.log`、`iteration-325-contracts-final.log`、`iteration-325-contract-scanner-final-tests.log`。

定向证据位于 `Switch/build/optimization/2026-10-10-iteration-325-*`：协调器真实 Unix IPC 与模拟物理 Owner 测试 21 项通过，包含未投递的 finalizer、返回成功但无新身份，以及实际提交后回复丢失三种不同结果。首次完整 Rust 测试发现新增 PC/SC 退休测试错误地要求业务结果成功；改为等实际退休屏障、断言结果未知，并另行等待真实 worker 结束与关闭。原失败日志保留为 `migration-gates-first.log`。

## 尚未退出 Go 的部分

Go 的物理生命周期适配、缓存桥及事务锁仍在；不能把本轮称为整个读卡器 owner 已退出 Go。除三个 pin 适配函数外，仍有以下 11 个函数使用 `authMu`：

- `reader_manager.go`：`beginCardOperationAdmission`、`observePresenceFromInventory`、`disconnectConfirmedHotplug`、`hotplugBLEInventory`、`connectContextWithInventory`、`DisconnectContext`、`loadUSIMInfoLocked`、`RefreshUSIMInfoContext`。
- `reader_pcsc_manager.go`：`ClosePCSC`。
- `reader_pcsc_presence_handoff.go`：`beginPCSCPresenceLoss`。
- `reader_vowifi_eap_session.go`：`respondNativeEAPAKAForSession`。

后续工作包须覆盖这些消费者及物理生命周期适配，移除 Go 事务协调和窄 pin 层后才称作完整读卡器迁移。IMS/IKE/VoWiFi 生产会话、N待网络会话、完整语音及实机长期验收另有剩余范围。零 Go 结构检查仍失败，不能用路由数或迭代号代表整体完成率。

本轮按原一致尺度计入 P2B **+0.100**；同时补记此前独立生产接管 **+0.225**，累计 **15.245 / 47.5 ≈ 32.1%**（工程估值，约 ±3 个百分点）。原 31.4% 为 Iteration 209 的旧核算值，详见[复核账本](optimization/all-rust/progress-reassessment-2026-10-10.md)。
