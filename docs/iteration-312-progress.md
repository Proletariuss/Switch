# Iteration 312：普通读卡操作接入 Rust 准入 Gate

普通 Go 卡片操作此前只受 Go `readerManager.authMu`、Rust PC/SC operation gate 和 BLE busy 标记保护；Rust N待 reservation 与原生 AKA/身份 worker 的 Gate 无法识别这些 Go 操作。本轮为普通、托管模式的 Go 卡操作增加 Rust 侧 reader operation lease：按当前物理 reader 名与 generation 申请，以唯一 operation token 做幂等重试，并在本地 PC/SC/BLE operation 释放后精确释放 lease。N待 owner 继续使用已有 reservation；已持有 Rust PC/SC transaction capability 的回调继续借用该事务，不重复申请。

Rust Gate 现在拒绝同一 reader 上重叠的 native worker、N待 reservation 与普通 Go operation lease，不同 reader 的普通 operation lease 可并行。Go API 中 eSIM 通知同步/重试、Profile 切换/改名/删除、下载与下载确认等操作遇到准入错误时停止提交并返回明确状态。退出阶段只额外允许精确的 operation lease release；确认 Go 子进程退出后 Rust 清理残留 external lease。错误释放会记录为 unconfirmed，不把超时误报成已释放。

回归审阅发现 N待已持有 reservation 后，profile 初始化步骤仍使用普通准入会自我冲突；该步骤已切到 owner-scoped 准入，继续复用它持有的 reservation。N待卡操作序列没有改为创建第二个 operation lease。

验证：Rust `reader_reservation` 定向测试 8 项通过；Go operation lease/读卡串行定向测试通过；完整 `scripts/test-migration-gates.sh` 通过（Rust workspace **1203 passed / 3 ignored**，Go vet/全包测试通过，既有 carrier/USB/radio/HTTP method+HEAD/SDP/AMR/IMS MESSAGE parity 通过）；Rust migration contract scanner 通过，清单为 **336 method/path contracts（Rust 275、Go 61）**、127 Swift HTTP 调用点、327 个审阅范围；`git diff --check` 通过。

边界：这只是普通托管 Go 读卡调用与 Rust Gate 的交叉准入，并未把 APDU、reader/session 生命周期或 `authMu` owner 移入 Rust。Connect/Disconnect/replacement、hotplug、identity/read lease finalizer、PC/SC teardown 等入口仍需汇入统一 reader actor；legacy 独立 Go fixture 未配置 Rust UDS 时保留旧兼容路径。故不提高全 Rust 加权进度 **31.4%**，也不声称 N待/读卡器全链路完成。没有访问实体卡、发送短信、拨号、运营商网络或生成 DMG。
