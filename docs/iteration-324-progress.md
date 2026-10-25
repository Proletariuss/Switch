# Iteration 324：Rust 持有蓝牙卡任务直到实际结束

2026-10-10。继续读卡器租约与事务管理迁移；没有生成 DMG，也没有操作真实卡片或访问运营商。

## 生产路径变更

Swift 蓝牙桥为 APDU/reset 分配可查询的操作记录，以 `operation_id + bridge_id` 绑定同一次请求。主线程排队、正在执行和终态分别保存；socket 等待超时不再冒充卡片操作结束。查询只读取受锁保护的记录，不依赖主线程，也不重发卡命令。完成记录有界，仍在执行的记录不因超时或缓存淘汰而删除。

蓝牙 manager 对已提交交换的失败增加实际结束证明：完整响应可以确认结束；写入、通知或 watchdog 错误必须等确切外设的断开确认或系统蓝牙关闭，才交付桥接终态。协议内部原有错误回调保持及时返回；B0/B1 认证参数未修改。

Rust BLE transport 在写请求后遇到超时、EOF、畸形响应或缺失证明时，仅查询原操作。必须匹配两项身份并得到明确终态，才释放物理占用；`pending/unknown/conflict` 和换桥响应均不算终态。该等待复用有界 blocking worker，不增加无限线程或重放 APDU。

读卡器 Gate 独立记录真实 BLE worker，Go 退出不会清除它。活动 worker 阻止业务租约释放、读卡器替换、新的同卡准入及 BLE 控制。生产接线包含：

- Go BLE gateway 在派发 blocking worker 前校验构造 transport 时固定的 Go producer，并原子登记占用；退出进程的迟到请求不会访问 Swift 桥。
- 原生 LPA 的健康扫描、下载、改名、总览、删除、通知列表/重试和切卡，以及公开 AKA，在进入容量队列前登记占用。
- 原生租约返回其确切 `native_authority`；Rust 在同一 Gate 锁内核对 producer、token、sequence、角色和选中 reader/generation。迟到的 acquire 响应不能借用后继租约。
- 租约内 BLE AKA 同样前置登记。取消尚未派发的任务可以释放；已执行的 blocking closure 独立持有 guard，等待者取消或超时不能提前释放它。

## 验证

- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,801 passed / 6 ignored**，daemon **1,272 passed / 3 ignored**；Go vet/全包测试、运营商/USB/radio/HTTP/codec/SDP/MESSAGE 差分通过。
- Swift 实际桥接 Unix server 与操作账本的假卡测试加入完整门禁；BLE manager 38 组事件序列通过。补入同外设重连保护后，两个 Swift 脚本再次通过，最终日志为 `swift-ble-manager-final.log` 和 `swift-ble-proof-final.log`。
- `xcodegen` 已将新增 ledger 纳入正式工程；实际 Release App 构建 `BUILD SUCCEEDED`，arm64 / 2.97，`codesign --verify --deep --strict` 通过。产物仅位于 `Switch/build/DerivedData/Build/Products/Release/Switch.app`；未启动、安装或覆盖 dist 中的发行产物，不能将其与冻结至 Iteration 322 的 2.97 DMG 混同。日志为 `app-build.log` 和 `app-codesign.log`。
- 契约清单核验通过：336 method/path（Rust 276 / Go 60）、127 Swift 调用点、347 审阅范围；扫描器 43 项测试通过。
- Rust gateway 定向 34 项通过；原生任务取消与占用 4 项、Gate/生命周期 40 项通过。Go producer/authority 定向并发测试及主包测试通过。
- 日志位于 `Switch/build/optimization/2026-10-10-iteration-324-*`；完整门禁为 `migration-gates.log`，native 任务证明为 `native-work.log`。初次 gateway 定向编译遇到测试缺少新 guard 参数，已补局部 Gate 夹具并通过复验；保留 `reader-ble-producer-first-compile.log`。

## 尚未完成的边界

Go 的 native lease token/计时、PC/SC 状态观察、`authMu`、物理适配和 IMS/N待在线会话仍在。此轮提供移走这些协调逻辑所需的物理占用证明，不称作完整 reader owner 已退出 Go。

下一完整迁移工作包的协议边界已交叉审阅：Rust 在首次 RPC 前分配不可变 lease ID/sequence 并持有 preparing/active/draining/released；Go 只固定既有物理适配器并返回同请求的不可变身份/AID/PCSC 回执。提前释放必须退休迟到绑定，未知结果按原 ID 查询/释放；切卡收尾须独立序号与回执去重。Rust 确認实际任务结束后才能松开 Go 旧消费者的物理 pin；Go 不再自行用时间流逝释放卡片。该协议尚未实施，不计完成。

Go gateway 的 producer fencing 不等于每次业务事务的 capability 传递；长寿命 Go 卡片适配器仍依赖原事务锁。Swift `connect/disconnect` 控制动作的主线程排队协议也尚未切换到本轮卡命令账本。桥接进程变更或记录未知时，当前行为保留占用；没有新增未经物理证明的超时强制释放或恢复协议。

总体百分比遵守纠正后的口径：**31.4% 是 Iteration 209 最后已核算基线，当前总体估值待一致尺度复核**。下一里程碑继续是 Rust 接管 native lease 生命周期、移除 Go watchdog/观察器和最终事务锁；接口数、测试数及本轮迭代号不换算为完成百分比。
