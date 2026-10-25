# Iteration 308：`authMu` 全部生产消费者清点

本轮按生产 Go 源码重新枚举 `readerManager.authMu` 的取得/释放点，确定下一项有真实切换价值的 P2B 工作必须覆盖 reader 生命周期、所有卡片事务、存在中的多待 reservation，以及 Rust lease 的代次和退出确认。单独迁移 BLE watchdog 或 `authMu` 一个调用方，会留下可以绕过新 owner 的生产入口。

## 当前 Go admission 消费者

| 文件 / 函数 | 受保护行为 | 必须继续成立的条件 |
| --- | --- | --- |
| `reader_manager.go` `beginCardOperation`、`beginCardOperationAdmission` | eSIM/LPA 与 AKA/其他 APDU 操作的互斥；与 Rust PC/SC operation 及 BLE busy 代次联动；普通调用还检查 N 待状态 | reader generation 与卡 operation 同一 owner；每个释放绑定真实 worker 退出；Rust session capability 调用不得重复加锁 |
| `reader_manager.go` `observePresenceFromInventory`、`hotplugBLEInventory` | BLE 状态探测期间与 APDU 串行；session lease 或 operation 中将 presence 视为 indeterminate | inventory 的迟到结果不得清理新 reader；probe 期间不得由 APDU 越过 admission |
| `reader_manager.go` `disconnectConfirmedHotplug` | 确认拔卡后 teardown exact reader/generation | 不得在 APDU/lease 活跃时 teardown；业务 session 清理成功才确认完成 |
| `reader_manager.go` `Connect`、`Disconnect` | 替换 reader、断开时关闭 card operation admission | 不能在旧代次卡操作仍运行时切换；close timeout 不等同于物理退出 |
| `reader_manager.go` `loadUSIMInfoLocked` | 初始化及刷新 ICCID/IMSI/MCC/MNC/ISIM 身份 | 同一代次下身份读取与 LPA/application select 串行；锁顺序 `rm.mu → authMu` 不得反转 |
| `reader_session_lease.go` acquisition、native callback、expiry finalizer | Go 迁移期 session lease 持有 authMu；PC/SC callback 暂借原 transaction；BLE lease 使用 Go watchdog | token、reader、generation、session sequence 一致；callback 退出和真实 worker 完成后才释放；到期只能使旧 owner 失效 |
| `reader_pcsc_manager.go` `ClosePCSC` | 退出时关闭物理 session 与 operation，并等待 lease watcher/finalizer | lease release、native PC/SC close 与业务退出确认顺序保持；不能把超时视作成功 close |
| `reader_pcsc_presence_handoff.go` `beginPCSCPresenceLoss` | physical presence loss 与 reader replacement/disconnect 串行 | 迟到 loss event 只能 retire 匹配的 producer/reader/session/sequence；IMS/N待 teardown 确认后才 ACK |
| `reader_vowifi_eap_session.go` `respondNativeEAPAKAForSession` | Rust-owned标准 EAP-AKA 的 Go PC/SC adapter 执行互斥 | capability 身份、application、reader generation 均需重验；token 不能授权 BLE 或替代物理卡所有权 |

## Rust 现有能力与缺口

Rust `reader_pcsc` 持有 PC/SC 句柄和 transaction；`reader_session_lease` 已有租约续期 worker；`reader_reservation::Gate` 目前保护 Rust blocking worker 与 N 待 reservation，但不是所有 Go reader consumer 的统一物理 reader actor。Go 仍保存 reader selection/generation、锁 admission、BLE expiry watchdog、业务 teardown 与 lease terminal 状态。因此当前 Rust gate 不能替代 `authMu`，也不能仅凭现有 Go mutex 测试宣布唯一 owner 已切换。

## 下一生产切换的验收顺序

1. Rust actor 使用稳定物理 reader identity 与 generation，提供 `acquire / renew / release` 和可取消的 operation permit；release 需要 producer token、lease、generation 三者匹配。
2. 将 reader connect/disconnect/replacement、presence/hotplug、USIM/ISIM/AKA、LPA/Profile 及 Rust native-session 全部改为该 actor admission。任何旧 Go 入口缺少 actor capability 必须 fail closed；compat path 不能跳过 reservation。
3. 对每类 operation 验证超时后 worker 仍在运行、reader replacement 与迟到 callback、lease expiry、Rust/Go任一进程重启、BLE bridge掉线、PC/SC close失败和N待reservation交错。只有底层操作真实退出并返回确认后才能释放 permit。
4. 先以 Go 旧 gate + Rust actor 双重 admission 做交错与 race 验证；确认全消费者路径后，再移除 Go `authMu`/BLE watchdog owner 和重复状态，避免提前出现双 owner 或无 owner 窗口。

证据边界：这是基于源码引用的生产消费者审计，无新 runtime owner、无卡片I/O、无实机验证，也没有移除任何 Go 依赖；不增加加权迁移完成分。9eSIM 6982仍排除；未生成DMG。总体仍为31.4%（约±3个百分点）。
