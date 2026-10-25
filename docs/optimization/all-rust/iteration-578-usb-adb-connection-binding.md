# Iteration 578：USB AT 到 ADB 的同连接绑定

日期：2026-10-19。整体仍为 **P1–P4 同步验收、P5 完善、P6 最终验收未完成**；294/294 只表示 Rust 接口归属。原方案要求、完整设备及运营商验收范围不变。本轮补齐“已打开原生 USB AT”分支的设备绑定，未完成共享模块 authority、串口绑定或重启后的身份验证。

## 生产行为与边界

`Transport::physical_target()` 现在从已打开的原生 AT handle 捕获其实际 `rusb::Device` 及 Context。返回的 opaque 凭据不能通过设备名称、VID/PID、地址、端口或 serial 字符串构造；默认不具备原生证据的 transport 明确失败，不额外打开设备或发送 AT。它在 AT handle 关闭后仍保留该 Device 引用；断开或重新枚举不能授权替换成另一台设备。

USB owner 同时捕获自身实例、producer、session、epoch 和 identity generation。在安装语音 transition lease 的同一状态锁内再次核对；捕获后新开连接、空会话经过开关后恢复为空、producer 变化或另一个 owner 均不能复用凭据。只读捕获本身不等于执行准入：停止中或已有 SIM lease 时仍可读取一致的元数据，但实际 transition acquire 继续执行原有停止、占用及租约检查。revoking 或 port/session 不一致时拒绝。

Native runtime preparation 在关闭 AT 前捕获，使用同一 capture 安装租约，再仅从该 Device 的实际描述符选择 ADB 接口。普通 route 在 preflight 前捕获并在实际 Start/Stop 前核对同一 capture。捕获存在时，目标或接口错误直接返回，不能进入无绑定的枚举选择。接口解析与原有选择共用同一实现，保留既有 alternate/端点规则。只读 Status 和 inactive Stop 仍不关闭 AT 或取得 transition lease。

另外修复继承的计数耗尽路径：Voice acquire 先完成 revoke 的两个 generation 增量校验，再安装 lease，避免失败时留下无法返回给清理 owner 的租约。

没有已打开 AT 的分支仍保留原有选择逻辑；这并不构成串口或跨请求的物理身份凭据。正常 enable/reboot/rebind 不会把旧 Device token 跨重启使用，而是在后续准备调用时重新捕获；**这也不等于重新枚举后的同设备身份验证已完成**。原生拔插、同 VID/PID 多实机和设备恢复仍待验收。

## 实际软件验证

冻结输入为 2,891 项，较 577 改变 9 项：7 个 Rust 文件及 2 个 API 元数据；人工复核更新三个生产作用域。完整 daemon 新增 23 项测试，当前 **2,578 passed、0 failed、0 ignored**。运行使用禁止真实硬件和外网的强制 OS 沙箱，canary、实际进程组退出及源码零漂移通过。详见[完整测试](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-daemon-full-result.json)。

新增测试覆盖目标证据缺失/错误、禁止替代枚举、ADB 接口解析、preflight 后 acquire 拒绝不执行 route、owner/session/代次竞争、已有 SIM lease、停止中只读捕获、真实阻塞 Drop 后的取消清理及计数耗尽。成功捕获和取消用例使用仅 `cfg(test)` 可构造的 Synthetic ownership fixture；它不含 USB 指针或 Context，native ADB 会明确拒绝，不能用来证明原生设备配对。

其他 Rust 工作区程序及 Swift suites 本轮没有重跑。576 尚未完成的 `tests::swift_forwarding_handoff_fixture_round_trips` 仍是 required pending；其精确程序保留，不能因为本轮 daemon 没有 ignored 而计作通过。原 PAC 普通配置失败与专项平台通过的历史证据也不由本轮覆盖。

[API 与 Go 退出门禁](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-default-gates-result.json) 通过：294 个 Rust 接口、54 个派生 HEAD、432 个审阅范围，129 个 Swift HTTP 和 2 个非 HTTP 调用。初次 inventory 驱动在人工审查更新进程结束前启动，读到旧摘要而失败；该[驱动失败记录](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-inventory-initial-driver-result.json) 保留，有序更新后检查通过，未修改检查器或降低条件。

## 候选、缓存与安装状态

实际生产 Release 后端入口构建通过；Swift 和资源未变，复用 577 GUI，没有声称新跑完整 Xcode/Swift 编译。对两个临时 GUI 副本去除签名后核对代码载荷相同，候选自身保留正常签名。见[构建及对照](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-release-backend-build-result.json)。

候选的 arm64、严格 ad-hoc 签名、许可、包内仅 Rust daemon/libusb 和无原生故障注入符号检查通过；包内 17 个场景、23 个实际进程、三种日志退出模式和最终正常 archive 的 10 种模式向量通过。这些仍是无硬件软件证据，不是公证、真实 GUI 操作或运营商验收。

候选为 `build/iteration-578-derived/Build/Products/Release/Switch.app`，Bundle ID 为 `com.proletarius.switch.candidate578`，包内后端 SHA-256 为 `b6ceb0bb89e104e087e4430279b9324c8a83deea63a76d6136f9bdd2c1ecff1b`。本轮没有替换安装应用或正式 dist，没有实卡、短信、拨号、切卡或下载操作，无 DMG。最近实卡观测仍是 574 的在位但 MUTE、ATR 长度 0，不是本轮复测。

全部测试及构建结束后，清理 `build/rust-target` 和空 `build/swift-cache`：分配空间 1,349,431,296 字节，同期可用磁盘增加 1,282,396,160 字节。前后 2,891 项输入及 12,180 个保留条目无漂移；源码、离线依赖、候选、EVS、报告、回退材料及 pending 精确程序保留。见[清理报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-cache-cleanup.json)。

## 继续推进的完整范围

[权限审议](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-gate-rights-audit.md) 确认不能直接动态化现有 Gate：radio 的 submit/fence/physical 及部分 voice/USB 分支会截断已受纳操作的 release、restore 或退出。后续需要分别落实新业务 OperationRight、设备/producer/operation 绑定的 SettlementRight 和 RecoveryRight，再接入共享单调撤销，并保留未结算的状态投影。本轮 Gate 仍保持原启动语义，没有把该审议计作实现。

[串口审议](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-serial-binding-audit.md) 定位了路径选择及重连遍历候选的现状。仍需以真实串口与 USB registry 实例建立受控绑定，并覆盖其发现、打开、重连和 ADB 操作；IMEI、port location 等辅助字段不能单独充当同一物理连接证明。

[跨语言前置审议](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-handoff-prerequisite-audit.md) 进一步区分 Foundation 偏好文件落盘限制和 Rust DTO 往返范围；仍须真实成功持久化及完整接口/重启链验证，不能改成内存 fixture 或手写文件冒充 Swift writer。

正常模块恢复、受控 legacy 生命周期、旧 3.42 退出 1、干净环境、原位升级回退、性能、当前候选硬件/运营商业务及自然长稳继续保留。最终证据见[578 门禁摘要](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-578-gate-summary.json)，整体目标保持进行中。
