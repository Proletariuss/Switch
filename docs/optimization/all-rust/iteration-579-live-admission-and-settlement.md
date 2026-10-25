# Iteration 579：共享准入撤销与已接纳任务结算

日期：2026-10-19。整体为 **P1–P4 同步验收、P5 完善、P6 最终验收未完成**。294/294 仅表示 Rust 接口归属；完整需求与适用设备矩阵尚未冻结为可计算的整体验收分母。

## 实现与边界

模块 Gate 分离不可变的启动隔离、共享且单调的新业务撤销，以及有限任务的结算权利。新准入与撤销在同一互斥点裁决；实际运行时仅通过已持有并校验的 daemon 数据目录句柄，以 nofollow 检查固定 pending 证据。文件即使不是有效 JSON 或之后被移除，也不能重新开放本进程的新业务；正常变化的 pristine/备份不作为逐请求自撤销依据。

非 Clone 的 OperationGuard 归实际有限任务所有，私有 OperationRight 只能证明同 Gate、同 producer 且原 owner 仍活跃。USB、串口、射频、QMI 和语音入口接入该权利；原设备、session、epoch、generation、租约及类型化计划围栏继续生效。这是逻辑权利，不是物理身份或恢复证明。普通 USB 兼容 SIM 长租约不能用于撤销后发起新命令；已经执行的整段事务和真实清理保留至 worker 完成。

模块卡仍先保留实体占用，再接纳 RF prepare，维持串口排他顺序。只有共享 intent 首次提交明确未进入队列的类型化 NotAdmitted 回执，才允许释放未使用的原占用。此回执永久封闭该 intent 后续 prepare；clone、取消、已经接纳及未知结果均不能制造该证明，仍须经过原 RF 恢复与实际释放。晚到的 SIM 身份新事务可被拒绝，现有 RF 清理继续可用。

语音 outstanding registry 跨请求保留原 ADB 计划、原 AT capture 及有限任务 owner。Start/Stop 的未知结果、取消和设备丢失均不能重新枚举其他设备；确认 Stop/音频回滚并完成 USB 实体释放后才能退休。最终退出先排空已接纳 worker，再检查完整 registry；没有本进程语音义务或只有已经清理完成的 prepared plan 时，不再额外派发 Stop。其他 producer 尚欠的义务仍阻止成功回执。未知 RF 下的 Stop pin 只接受真实原义务的 active right。

原生 USB 使用稳定且可重试初始化的 libusb Context，同连接比较保留的 Device 实例；设备路径、地址、VID/PID 或 serial 不能替代它。该实现仍不证明重启、重新枚举、未打开 AT 或串口到 USB/ADB 的同设备关系。通用 reconnect 的现有选择行为没有因此获得跨设备身份保证。

## 软件验证

当前冻结 2,892 项输入，较 578 改变 37 项（35 个 Rust 文件、2 个 API 元数据），新增的生产串口 admission 模块纳入人工指纹审阅。完整 daemon **2629 passed、0 failed、0 ignored**，新增 51 项；强制硬件/外网拒绝沙箱和 canary 生效。见[完整结果](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-579-daemon-full-result.json)。

首次完整结果 **2605 passed、24 failed** 及初次编译错误均保留。失败涉及旧测试夹具分别创建不同 Gate、一个 Refresh 策略断言写反，以及 host 已收到 PCM 但发送计数尚未发布的测试竞态。六个测试文件修正后重新冻结并重跑全量：统一真实共享 authority；媒体测试在有限时间内等待同 call/token/generation 的四项精确统计，超量立即失败，原精确断言不变。未修改生产计数顺序、产品超时或身份检查。见[失败审议](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-579-first-full-failure-review.json)。

API/生产 Go 退出门禁通过：294 个 Rust 接口、54 个派生 HEAD、433 个审阅范围、129 个 Swift HTTP 及 2 个非 HTTP 调用。其他 Rust 工作区程序及 Swift suites 本轮未重跑；required `tests::swift_forwarding_handoff_fixture_round_trips` 仍待真实成功持久化及重启链验证，不能用当前 daemon 的零 ignored 代替。

## 产物与实机状态

候选为 `build/iteration-579-derived/Build/Products/Release/Switch.app`，标识 `com.proletarius.switch.candidate579`。SwiftUI 与资源未变，复用 578 GUI，实际执行生产 Release 后端配方；本轮没有声称重新完成完整 Swift/Xcode 构建。

用户授权强制退出的 PID 4114 在本轮核对时已经不存在，没有对其他 PID 执行强制退出。安装应用、正式 dist 和原始项目未修改；没有拨号、发信、切卡或下载，未生成 DMG。当前候选没有新的实机/运营商验收。最近实卡证据仍是 574 ESTK 在位但 MUTE、ATR 长度 0，不是本轮观测。

实际候选严格 ad-hoc 签名、arm64、许可、仅Rust/libusb后端及无原生故障注入符号通过。17场景23进程、三种日志退出、最终正常EVS archive的10模式向量通过；额外live场景在干净目录启动后加入pending、移除后仍三次拒绝新业务，reader缓存/状态保留，SIGTERM实际0退出并只清理自有socket。首次live驱动误将启动隔离的全局409套用于AT兼容层，失败保留；v2按既有502/at_transport_failed/完整撤权reason/recoverable=false严格检查，不接受任意502，生产错误语义未为驱动修改。HTTP观测不充当硬件调用trace，零factory测试和强制OS沙箱另列。见[live完整结果](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-579-live-revocation-result-v2.json)。

后端SHA-256：`d91c22df821feb666a4228ae75a696ad510ea74955f6e864427ec31e23fa02d4`。全部验证结束后精确清理`build/rust-target`与空`build/swift-cache`，分配空间1,370,509,312字节，同期可用空间增加1,319,788,544字节；2,892项输入及13,153个保留条目零漂移。待验收的精确handoff程序、源码、离线依赖、EVS、候选、正式dist及失败证据保留。见[清理报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-579-cache-cleanup.json)。

## 剩余完整验收

显式 Startup RecoveryAdmission、受控 legacy/shared guardian 生命周期、未受控旧版与已接纳任务并发、串口/无 AT/重新枚举物理绑定仍未闭合。真实跨语言落盘、旧 3.42 退出失败、原位升级回退、干净环境及整机性能、当前候选实卡注册/短信/语音、多 Profile 及 24h/72h 自然长稳继续保持未完成。全方案范围没有因本轮软件门禁通过而缩小。

最终完整软件证据见[579门禁摘要](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-579-gate-summary.json)，项目目标保持进行中。
