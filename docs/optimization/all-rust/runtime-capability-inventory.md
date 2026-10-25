# 全 Rust 迁移：非 HTTP 运行时能力清单

当前核对（2026-10-25，3.51/build351，`source-not-final-G0B`）：现行审计为312条method/path（Rust312/Go0，59条HEAD别名）、130个Swift HTTP调用点；来源/库存审计仍exit1，诊断与旧库存保留，未采纳最终successor。Rust runtime 的 forwarding、N待 recovery/maintenance/retry、BLE/presence/hotplug、module startup 与清理定时器由 `proletariusd/src/main.rs` 启动，退出在其 native/module/reader/QMI 协调分支汇合。Rust业务、Swift系统桥、codec FFI和原始证据见[当前运行时所有权](README.md#current-runtime-ownership)。非HTTP能力仍需各自运行证据，不能只据路由数标记完成；G0B/G8/G9仍NO_GO、正式长稳0小时，不冻结总分母或百分比。

> 以下为各时期原文，包括Iteration665以前的“最新/当前”、Go路径/归属、构建阻断、计数与能力表；这些均是历史快照，**不是当前仍运行的owner或当前验收结果**。保留原日期、行号和失败，不改写历史表。

RT-H02 本轮实际修复为 `R/diagnostics.rs` 的有界日志 writer 与 `R/main.rs` 四个有界 exit drain，满/闭 OS 管道下真实 daemon API 和退出通过；Swift `BackendProcess` 仍是日志系统桥，未迁移 UI。RT-H03/V6 的本地备注由 `R/profile_notes.rs` 三个 Rust writer 保留精确未知扩展；这只验证该文件，不能推及所有配置格式。见 [Iteration564](iteration-564-notes-and-nonblocking-diagnostics.md)。P5 编码重协商、P6 生命周期 admission/旧回退与实机/长稳/性能等剩余项见 [565审议](iteration-565-p5-renegotiation-residual.md) 和 [561残余计划](iteration-561-p6-residual-plan.md)。

最新源码结构核对（Iteration505，2026-10-16）：Go reader manager、PC/SC/BLE逻辑卡消费者、reader lease/receipt/auth/identity客户端和Go shutdown reader回调已退出生产构建；Rust对应15个私有HTTP端点退休，原生typed物理owner、Gate、生命周期、AKA、公开identity/authenticate及N待保留。`reader-business`仅保留供Go模块eSIM准入使用的status只读观察，不能获取、转移或释放能力。SMS reader facts在所有模块模式下均由Rust当前生命周期/IMS提供；Go只提供serial/nonmanaged模块facts。源码与契约审阅不等于本轮全量软件门禁或实机验收，最终执行结果以Iteration505报告为准。

最新静态退出检查仍有6类构建/运行时阻塞：Xcode编译Go、App嵌入Go可执行文件、Rust监督Go、Rust派发给Go、Go module与生产Go源码。相同扫描口径（backend/cmd/proletarius-macos + backend/internal，排除测试/vendor/third_party）从208降至195个文件。实际macOS daemon `go list -deps`从298降至288包：reader-only estkble、uicc usim/mbim/qcom子图退出；Go ADB/QMI的CGO/libusb及模块eSIM apdu仍在。goscard/gousb/purego/旧IMS包在505基线已不在实际daemon依赖中，不能重复记为本轮收益。

契约300条（294 Rust HTTP owner、6 Go），含54个GET派生HEAD；仍有76条Go owner/dependency，详见当前`api-contract-inventory.json`与505审阅记录。公开reader connect/disconnect/refresh/auth及native-session-plan经传递调用审阅使用原生Gate/supplier/physical owner；reader eSIM的6项操作仍在非native-managed模式读取Go模块`GET /api/esim/operation`。下面保留的历史能力表和旧Go文件行号是各轮快照，不能据其描述推定已退休owner仍存活；当前reader子图以[505 Go退休报告](../../../../build/reports/iteration-505-go-reader-retirement.md)、[Rust wire退休报告](../../../../build/reports/iteration-505-rust-wire-retirement.md)和[契约审阅报告](../../../../build/reports/iteration-505-contract-review.md)为准。不重新计算加权完成率，不把文件、路由或测试数量当作完成分数。

初始核对日期：2026-10-04；2026-10-09 Iteration 203 新增 `proletarius-vowifi::ims_network_runtime::ImsNetworkRuntime`，把动态 TCP registry 与 IMS security runtime 组合到一个 tunnel network owner；目前未连接 native keyed session actor，不改变当前生产 owner。Iteration 189-195 更新DTMF/拨号/接听/挂断公开准入、通话状态公开响应投影、模块通话snapshot归约、选中BLE AKA身份检查、IMS OPTIONS心跳恢复策略及N待短信发送readiness决策进入Rust，Go仍负责实时硬件观察/动作与会话适配；Iteration 188 更新模块固定拨号/接听/挂断/DTMF AT 命令构造与串口执行进入 Rust modem actor、Go保留SIM/USB准入及呼叫生命周期边界；Iteration 187 更新模块 `AT+CLCC` 查询与状态解析进入 Rust modem actor、Go保留SIM互斥/URC兜底/通话记录边界；Iteration 180 更新短信设备选择、选中BLE eSIM下载、IMS OPTIONS heartbeat及读卡器信息投影边界；Iteration 172 更新N待维护调度与短信队列接纳owner，Iteration 176 更新公开设备状态投影与模块 AT 串口发现owner，Iteration 177 更新受监督串口/USB状态的PLMN显示名投影owner；2026-10-08 Iteration 168-171 更新N待恢复策略owner、逐Profile持久状态、启动协调及运行状态归一化；2026-10-05 Iteration 095 更新USB手动存储/删除授权与精确归档边界，Iteration 094 更新USB完整状态采集/缓存边界，Iteration 093 更新USB AT句柄/事务、语音门禁与本地短信采集，Iteration 092 更新USB自动收件/分区预览条目，Iteration 091 更新通话事实/后台观察条目，Iteration 090 更新串口初始化/就绪条目，Iteration 089 更新模块收件及退出条目，其余条目保留各自历史核对范围。范围为 Switch 当前源码，配合 `../2026-10-04-all-rust-backend-plan.md` 与同目录 API 清单使用。本清单负责接口背后的生产者、定时器、设备占用、写入者和退出路径，不以 HTTP 路由数量代替迁移完成度。

终态是单一 `switchd-rs` 后端；Swift 保留界面、系统通知和必要的 CoreBluetooth/CoreAudio/Keychain 系统桥。业务准入、持久化游标、协议状态、卡片归属与业务重试归 Rust。过渡期现有 Go 仍是多项能力的唯一生产 owner；Rust 库存在、离线向量通过或能构造 REGISTER，均不等于实际接管。

最初审计仅只读生产源码，没有运行服务或读取生产数据。2026-10-04 后续离线实施增加了分离监听与并发准入，使用合成数据和 Go/Rust 测试后端验证；未接触号卡、发送短信或读取生产数据。不以解决所有 9eSIM `6982` 错误作为全 Rust 完成门槛；用户后续追加的 B0/B1 密码及 B0 兼容认证另行实施并发行；正常 LPA、模块控制、现有完整语音仍属于迁移范围。

Iteration479 对 RT-B08 的补充：Open 在 actor 内原子返回真实 serial_session；Go Close 和能力失败清理只携带不可变 expected session，Rust stale Close 幂等 ACK 不影响新串流、初始化或 pending read。Native call-mode rebind 先确认 IMEI，再恢复 required 初始化并执行固定队列，确认原 receive preference 后才提交 binding；deadline/epoch cancellation 贯穿探测与每个初始化步骤。此 Close fence 不声称所有 generic modem RPC 或 Go 运行时已经退出。

### P1 实施更新：请求入口隔离

- Swift 公共请求、Go→Rust 原生计算回调、多待/短信账本内部操作分别通过独立的 `0600` UDS。Go 子进程收到三路约定中的计算回调和内部账本路径；内部客户端使用 `SWITCH_INTERNAL_SOCKET`，旧独立 Go fixture 在缺少该变量时仍沿用旧socket。
- 各 listener 以独立 `JoinSet` 限制活动服务任务：公共64、计算回调32、内部账本16；Go 退出期间两个必要通道共用16条 cleanup 任务预算。所有通道的HTTP头期限为5秒。该数量是已 accept 并服务的任务上限，不等于内核等待队列或请求体全局上限。
- 实际 Go/Rust 进程夹具同时占满公共与计算回调任务，并验证额外请求未抢占名额、账本操作仍通过独立socket完成；Rust daemon 217项单测及15项双进程契约检查通过。仍标记 H0：未作实机、长稳或业务接管验证。

## 路径和证据约定

下列路径均相对 `/Users/proletarius/Desktop/Switch/src`。行号是本轮核对时的定位，后续编辑应以函数名重新定位。

| 简写 | 目录 |
| --- | --- |
| G | `backend/cmd/proletarius-macos` |
| B | `backend/internal` |
| I | `backend/internal/vowifi/imscore` |
| W | `backend/third_party/swu-go/pkg/swu` |
| R | `rust/crates/proletariusd/src` |
| C | `rust/crates` |
| S | `app/Sources` |

每行“证据”使用下面的离线夹具组编号，以及实机标记 `H0`。`H0` 表示**当前全 Rust 接管尚未在本轮实机确认**，不否认已有 Go 功能历史运行；不能写成已通过或无法支持。`E*` 表示已找到对应测试源码，仍须按目标源码 hash 在隔离目录重跑；`V*` 表示下一阶段需补充的验收。本文件不提供迁移百分比。

接管状态需分别记录：实现存在 → 离线验证 → 实机验证 → 生产切换 → Go 实现/构建依赖移除。现在的 Rust 原生库状态和 P1 新增代码不跨越这些门槛。

## A. 进程、后台任务与退出

| ID / 能力 | 真实入口及触发 | 当前 owner / Rust 基础 | 接管门槛 | 证据 |
| --- | --- | --- | --- | --- |
| RT-A01 应用启动与 daemon 存活 | `S/ProletariusApp.swift:199 applicationDidFinishLaunching`；`S/BackendProcess.swift:148 start`、`:185 terminationHandler`、`:233 beginReadinessCheck`、`:277 stop` | Swift 启动 Rust daemon，维护启动代际、日志 drain、readiness；这是系统进程桥，终态保留 | 不以窗口关闭等同退出；迟到 readiness 不复活旧进程；Swift 重启后台时不复制业务重试；stdout/stderr 必须持续 drain | E1 / H0；V1 |
| RT-A02 Go 子进程监督与恢复 | `R/main.rs run_with_sms_stores`：250ms child monitor、500ms parent monitor、SIGTERM/INT、受限重启；`R/gateway.rs GoBackendProcess::spawn/terminate` | Rust 已在线监督 Go；迁移完成才删除 Go 启动和监督，不提前删错功能 owner | 原生 actor 退出和恢复替代 child-exit 语义；后台异常不能清空 store；应用父进程退出无孤儿；最终包无 Go 可执行文件/动态库 | E1、E5 / H0；V1、V8 |
| RT-A03 后台生产者初始化顺序 | `G/main.go initSMSArchive`、`G/usb_at_controller.go configureNativeUSBSMS`；`R/usb_sms_receive.rs Service`；`B/modem/manager.go Start` | Rust在Go ready且本代SMS配置确认后进行USB单worker采集，未配置时不打开设备；Go安装设备初始化/非短信回调并提供serial/adopt/interval配置，config_seq阻止迟到旧配置；串口仍由Rust执行六步初始化及CMTI接收，Go完成设备采集并通过Rust RF启动屏障后请求scan | 先恢复存储再准入；配置/producer换代不能让旧任务绑定新卡；硬件初始化完整迁移仍待完成 | E5、E6 / Iteration093离线配置乱序及native采集测试；H0；V3 |
| RT-A04 通信退出汇合 | `G/communication_shutdown.go`；`G/call_mode_lifecycle_client.go`；`R/call_mode_lifecycle.rs shutdown_producer/retire_producer/finish_shutdown`；`R/main.rs run_with_sms_stores`；`R/usb_at.rs shutdown/finish_revoke` | Rust 模块通话模式退出捕获 producer、持有 shutdown_gate，取消旧代并等待 admission/worker/read/release owner 真正排空，实际 native stop 与 USB 清理确认后才 ACK；监督重启先退休旧代。Go 只调用并传播结果，其余通信退出职责仍逐项迁移；串口 Close 后 join 短信 worker | 已取得 lease 的早退及 refresh 均保留清理 owner，以原 identity 重试 release；队列满/失联不假报释放。真实 stop 不因 HTTP 断开或超时被当作完成；驱动与硬件退出仍未验收 | E1、E3、E7、E11 / Iteration479 生命周期与 owner 源码审阅；历史 Iteration093 取消/Drop 夹具；H0；V1、V4 |
| RT-A05 退出期间内部收尾写入 | `R/main.rs serve_internal_shutdown_connection` / `serve_shutdown_callback_connection`；`R/usb_at.rs command/configure/release` | Rust为账本/cleanup与EAP保留认证通道；USB AT保留同producer资源释放及CFUN查询，RF恢复仅经module-radio原token的typed intent，拒绝短信/APDU与新配置；公共准入已关闭 | 不中断已提交事实耐久收尾；旧token不能释放新事务；实际I/O返回后才释放占用 | E1、E5 / Iteration093内部HTTP/关闭边界测试；H0；V1、V3 |
| RT-A06 UI 定时读取、唤醒与通知 | `S/SMSStore.swift:121 startPolling`；`S/MultiStandbyStore.swift:127 setServiceRunning`（3s）；`S/ProletariusApp.swift:287 didWake` | Swift 可继续 UI 轮询与系统通知；业务自动恢复要移到 Rust | UI 不打开也应接收、续期与转发；展示游标独立于转发/通知游标；旧刷新结果不覆盖新启动代际；wake 只发事件，由 actor 决定恢复 | E8 / H0；V4、V5 |

## B. 设备控制与卡片归属

| ID / 能力 | 真实入口及触发 | 当前 owner / Rust 基础 | 接管门槛 | 证据 |
| --- | --- | --- | --- | --- |
| RT-B01 读卡器发现与热拔插监测 | `R/reader_inventory.rs handle/reconcile`；`R/snapshot.rs:18 capture`；`G/reader_hotplug_monitor.go:27 poll`、`:61 run`、`:77 startReaderHotplugMonitor` | Rust PC/SC 枚举已在线，Go 仍确认拔卡并退出会话；BLE 名单来自 Go/Swift | 单次枚举缺失不破坏有效会话；独立后台热拔插检测不能依赖 UI GET；两条 PC/SC 枚举路径共享执行预算；掉卡退出对应代际 | E2 / H0；V2、V4 |
| RT-B02 卡片连接、复位和逻辑会话 | `R/reader_reservation/lifecycle.rs`；`R/reader_pcsc/lifecycle_binding.rs`；`G/reader_manager.go` | Rust 持有 selection/generation 和不可变转换回执；Go 投影确认状态并执行物理生命周期适配。未提交或悬挂的切卡收尾在实际工作结束后失效旧身份；已提交新代不因回复丢失回滚 | 普通/生命周期事务已使用 Rust 精确能力，全部 authMu 已移除；物理生命周期与身份适配仍待迁移；不同读卡器独立运行及实机验证仍缺 | Iteration323/325/326离线通过；H0；V1、V2 |
| RT-B03 USIM/ISIM 读取与 AKA | `R/reader_auth.rs`；`R/reader_session_lease.rs`；`G/reader_manager.go` 身份事实适配 | 原生 PC/SC/BLE AKA 与 LPA 统一使用 Rust 租约及真实 worker 占用；Iteration298 已退役 Go 私有 AKA lease 提交接口。Go 仍提供现有 USIM 快照和旧业务适配，Swift 保留 CoreBluetooth 系统桥 | 卡片身份和代次核验、AUTS、取消后真实完成；完整原生 IMS 会话和实机尚未接管/验收 | Iteration298/324/325/326离线通过；H0；V2 |
| RT-B04 跨进程 lease 与 reservation | `R/reader_session_lease/coordinator.rs`；`R/reader_reservation/native.rs`；`G/reader_session_lease.go` 窄 pin 适配 | Rust 首次RPC前分配身份与序号并持有准备/活动/收尾/释放、续期和到期；旧 Go token/TTL/watchdog/续租协调器删除。Go 只固定遗留消费者、回传事实及一次性物理收尾回执；未知响应只查询/退休原请求 | 真正结束卡片worker和PCSC句柄后才释放；Go退出不跳过证明；Go窄pin/物理适配仍在；普通事务仲裁已由Rust接管；完整reader actor与实机验收未完成 | Iteration325真实Unix与模拟物理Owner、Go race、完整离线门禁通过；H0；V1、V2 |
| RT-B05 BLE 平台桥与字节传输 | `S/ReaderBLEBridge.swift`、`S/BluetoothReaderManager.swift`；`R/reader_ble_apdu.rs`、`R/reader_ble_gateway.rs` | 卡片APDU/reset通过统一Rust工作流能力、物理许可及精确reader/producer进入Swift桥；桥的operation ID/epoch与真实结束账本支持原请求状态查询。Swift保留CoreBluetooth/GATT/分包认证；已提交未知结果不重发 | BLE connect/disconnect控制的实际终态协议及系统桥恢复边界仍需完善；卡片传输接管不表示原生IMS完整就绪；实机重连/退出待验收 | Iteration321/324/325/326离线通过；H0；V2 |
| RT-B06 BLE 连接恢复策略 | `S/BluetoothReaderManager.swift:373 connect`、`:408 disconnect`、`:446 armPhaseDeadline`、`scheduleReconnect` | 目前 Swift 还含连接选择、重试与会话状态；不能把整类当“薄桥”而原封不动保留业务 | 区分系统级链路恢复与 Rust 业务重连；手动断开不自动复活；重新扫描不切换当前读卡器；BLE 阻塞与卡片 actor 退休一起验收 | E9 / H0；V2、V4 |
| RT-B07 模块 USB / AT / QMI 控制面 | `R/usb_at_transport.rs`、`R/usb_at.rs`、`R/usb_at_status.rs`、`R/usb_at_storage.rs`；`G/usbat_rpc.go`、`G/usb_at_controller.go`、`G/usb_at_device_status.go`；`R/qmi_enhancement.rs` | Rust独占实际USB AT句柄、普通/prompt I/O、事务lease与本地SMS快照；USB完整状态AT计划、16字段投影（含 operator_identifier/operator_name）及RF关闭展示也由Rust执行，同一lease内采集并确认后才发布缓存。手动存储用量/清空/单条与发送方删除也由Rust同session事务执行，Iteration177起受监督serial typed operator query与USB状态的PLMN→显示名映射由Rust共用投影生成，Go适配器消费operator_name；Go QMI/MBIM与独立兼容映射仍保留。Go仍保留已存号码及业务调用适配；AT诊断公开命令校验、RF准入、设备响应分类和用户可见失败语义由Rust执行，Go只保留跨业务SIM/APDU互斥锁及原始错误/响应桥；QMI libusb及typed IMS/IMSA/NAS/UIM也由Rust持有。Go已无AT libusb实现，仍保留业务选择、eSIM/AKA/语音编排及ADB传输 | 设备插拔、驱动阻塞、语音重枚举及实体差分仍待验收；所有业务和ADB迁移后才可退出Go | E6 / Iteration094固定计划/会话/取消测试、85组Go差分和Go race；Iteration093假I/O；QMI见Iteration076/084；H0；V2、V7 |
| RT-B13 模块 SIM/APDU 跨业务仲裁 | `G/main.go:runATCommand/runModuleCallCommand`；`G/device_sms_send.go`；`G/module_vowifi_sim.go`；`G/usb_at_device_status.go`；`G/usb_sms_storage_operation.go`；`G/at_console_policy.go`；`R/modem_actor.rs`；`R/usb_at.rs` | Go `moduleSIMMu` 仍是跨业务长事务门禁，并与 USB-AT session lease、`voiceUSBTransition` 组合；Rust modem actor 每条 AT 串行、USB owner 独立串行自己的 handle，但尚无共同覆盖 serial/USB 多命令序列的物理卡 lease | 建立单一 Rust arbiter 并迁移所有 Go/Rust 操作入口；lease 绑定 owner/generation/token；串口与 USB 共用同一张卡的真实资源身份；取消/子进程退出不得提前释放已提交 I/O；避免 Rust→Go→Rust 嵌套获取死锁；未知写结果保持冻结不重放 | E6 / Iteration305生产调用清点与锁顺序风险审阅；完整交错、取消、旧 owner、热拔插/超时测试及硬件验收待做 |
| RT-B08 AT 接收循环、URC 与 watchdog | `B/modem/manager.go Start/readLoop/handleURCObservedWithEvents`；`R/modem_actor.rs run_actor/process/frame_input`；`R/modem_actor/sms_receive.rs` | Rust独占serial handle、分帧、命令优先级/响应关联、AT事务及CMTI观察与接收队列；frame携带原始generation和收件归属。Rust另负责六步初始化、同会话缓存/关闭取消、独立能力协商及PDU就绪；初始化连续超时由Rust关串口并保留typed watchdog恢复原因。Iteration176 起正常受监督启动的串口候选枚举及有界 `AT`/`AT+CGSN` 探测由 Rust modem actor 执行并关闭临时句柄；Go只接收所选 `/dev/cu.*` 路径，显式独立 Go 兼容启动仍保留旧发现回退。Iteration177受监督串口operator typed query的PLMN名称解析也已在Rust执行；Go QMI/MBIM与独立兼容路径仍保留映射。Go保留设备采集/radio协调、健康镜像及SIM/USSD/语音等非短信回调 | 各硬件路径还需实机与长稳；非短信业务owner仍须迁移。关闭要等真实ACK和worker结束，不因请求超时复用端口 | E6 / Iteration090：初始化时序/失败/关闭/能力协商/就绪/watchdog伪串口测试和Go race通过；Iteration089：队列/代次/退出测试通过；H0；V2、V7 |
| RT-B09 模块自动飞行模式与恢复 | `R/module_radio.rs`、`R/module_radio_store.rs`、`R/module_radio_transport.rs`；`R/modem_actor/radio.rs`；`G/module_radio_client.go` | Rust唯一写偏好，持有公开GET/POST、启动屏障、3秒worker、业务预约、完整USB/serial CFUN计划与原生探测抑制门禁；Go原writer和timer已删除，只留调用适配。稳定RF-off不重复保存 | blocked bootstrap不得伪成功；固定会话、预算、取消后真实join、旧producer逐步撤销及未知结果不重放；实机/长稳仍缺 | E6 / Iteration096：86组旧Go raw差分、9错误阶段、PTY/假I/O、Go race及启动/恢复回归；H0；V2、V7 |
| RT-B10 模块 VoWiFi 的 radio / SIM lease | `R/module_radio.rs intent`、`R/usb_at.rs radio_execute`；`G/module_radio_lease.go`、`G/module_vowifi_api.go`、`G/module_vowifi_sim.go` | Rust持有长期RF intent、最新偏好恢复策略和原生收件inhibit，短时AT/SIM占用独立；Go仍编排AKA/IMS及opaque intent调用，不能视为VoWiFi已去Go | 恢复原binding且实际确认才释放；清理先到不得复活；未知结果换token也不得重放；真实退出、换卡和重枚举另验 | E3、E6 / Iteration096生命周期/取消/代次/恢复回归；H0；V2、V4、V7 |
| RT-B11 正常 LPA 长任务及确认 | `R/reader_esim_download.rs`；`R/module_esim*`；`C/proletarius-lpa`；Go eSIM观察/物理收尾适配 | 普通USB模块及选中PCSC/BLE reader的下载主要路径、大小确认与卡命令已接Rust；原生读卡器租约由Rust协调。Go仍负责部分VoWiFi安全停止、operation observer、非USB兼容和物理刷新收尾，不能称作完整LPA退出Go | 保留大小/取消/进度/通知/STORE DATA/身份确认；后台任务独立于HTTP；未知写卡不重放；全卡兼容及真实运营商验收仍缺 | Iteration180/209/325离线证据；H0；V2、V7 |
| RT-B12 卡资料缓存与长期偏好 | `G/main.go:3909 persistProfileNotesLocked`；`G/reader_vowifi_api.go:654 saveIMEI`；`G/main.go:2075 persistSMSAdopt`、`:3494 persistVoiceEnabled`；carrier override store | Go 保存 note/IMEI/采纳/voice 配置；Rust carrier overrides 仅覆盖对应配置子集 | 每份文件列出唯一 writer 与既有 schema；迁移不重置偏好、不把其他卡历史套给当前卡；临时写入与失败可恢复；保留文件权限 | V6 / H0 |

## C. IMS / VoWiFi / IKE / IPsec

| ID / 能力 | 真实入口及触发 | 当前 owner / Rust 基础 | 接管门槛 | 证据 |
| --- | --- | --- | --- | --- |
| RT-C01 会话建立与发布 | `G/reader_vowifi_session.go:368 start`、`:757 launch`、`:1031 run`；`R/native_vowifi_session.rs handle` / `register_ims` / `start_tunnel` | Go 生产运行；Rust startup/registration supervisor 已有离线组成，但 native handler 仍受未满足 gate 限制 | 不能直接翻 enable 开关；只有 IKE + protected IMS + live SMS event loop 同代成立才发布 ready；失败释放资源，不误报可发信 | E3 / H0；V3、V4 |
| RT-C02 IKE SA_INIT、EAP-AKA、认证 | `W/session.go:230 Connect`、`:283 connectOnce`；Rust `C/proletarius-vowifi/src/{startup,sa_init,auth,eap_runtime}.rs` | Rust 有真实协议/crypto/loopback 实现；线上网络会话仍由 Go 持有 | 身份与密钥绑定到同一 profile/epoch，保留运营商 EAP-AKA/AUTS；DNS/候选 ePDG/failover/fragmentation/重传与错误映射对照现行为 | E3、E4 / H0；V3、V4 |
| RT-C03 外层与内层 ESP / 用户态网络 | `W/dataplane_netstack.go:66 startNetstackDataPlaneLoop`；Go ipsec 包；Rust `C/proletarius-ike/src/esp.rs`、`C/proletarius-ims/src/security_esp.rs`、`C/proletarius-vowifi/src/netstack_runtime.rs` | Go 外层 SWu + IMS security 生产，Rust 已有算法和网络基础 | 无效 MAC/畸形包不污染 replay window；并发同序列只一次；序列耗尽停发换 SA；双向 selector/SPI/port 正确；RX/TX 全部 join 后释放 | E3、E4 / H0；V3、V4 |
| RT-C04 保活、网络变化、rekey、reauth | `W/session.go:631 startIKEReauthTimer`、`:735 startNetEventMonitor`、`:787 startNATKeepalive`、`:862 startIKESARekeyTimer`、`:928 startChildSARekeyTimer` | Go 有生产生命周期；Rust `auth.rs` 部分 authenticated control exchange 对未支持消息明确报错，不能宣称完整 rekey | 分别标明 IKE rekey、CHILD rekey、reauth 支持策略和验收；不以重建冒充无损 rekey；rekey 不等于免除 EAP-AKA；睡眠/网络改路与计时器绑定会话代际 | E3、E4 / H0；V4 |
| RT-C05 protected REGISTER / Security-Agree | `I/register_session.go`、`I/register_transport.go`；Rust `C/proletarius-ims/src/{register_request,register_auth_progress,security_offer,registration_connection}.rs` 与 authenticated IMS codec | Rust生产接管 REGISTER 候选配对/地址族与ePDG排序、wire编码/解析、事务身份/action、challenge解析/选择、AKA Authorization组装、Security offer及重试策略；Go仍管理REGISTER FSM、TCP socket/事务、候选拨号/探测与实际重试、物理AKA及IPsec transform安装 | AKA nonce/security SA/端口与注册世代匹配；保留挑战与 200 的字段验证；注册完成不能自动意味着短信/语音完整；IMS lifecycle 和设备/运营商验收仍待迁移/验证 | Iteration361–382离线门禁通过；E3 / H0；V3 |
| RT-C06 注册后单接收器、SIP 事件分派 | `I/transport_runtime.go:307` writer/PortC reader/PortS listener/OPTIONS，`:864 subscribeResponses`、`:916 dispatchIncomingRequest`；`I/sms_runtime.go` | Go 仍拥有生产 event loop；Rust `RegisteredSipRuntime` 已提供有界 frame 解析、Port-C/Port-S连接标识和原连接回应，但暂无生产调用者，响应关联、MESSAGE/RP-ACK、OPTIONS/NOTIFY、呼叫分派与actor关闭仍未接管 | 每连接一个 reader；响应按 session generation + Call-ID + CSeq + method 分派；MESSAGE、RP-ACK、NOTIFY、OPTIONS、对端断开及 PortS 响应回写完整；队列有界且可取消 | E3 / H0；V3、V4 |
| RT-C07 IMS 注册租期与重连 | `G/reader_vowifi_session.go:1330 watchIMSRuntime`、`:1346 watchRegistrationLease`、`:1457 reconnectAfterFailure`；`R/vowifi_session_policy.rs` | 当前 Go 到期标记 planned_renewal 后重建；部分退避策略计算在 Rust | 保留可观察租期原因、重连次数、停机区间；同会话 REGISTER 性能优化不是迁移先决条件，也不在此阶段重新启用；实际退出完成后才允许新代启动 | E3、E7 / H0；V4 |
| RT-C08 退休代与资源释放 | `G/reader_vowifi_session.go:1626 cleanupResources`、`:1667 stopVoWiFi`、`:1723 awaitRetiringVoWiFiSession`、`:1746 waitForVoWiFiSessionExit`；`R/native_vowifi_supervisor.rs:242 worker_finished` | Go active/starting/retiring + Go runDone/swuDone；Rust 纯 supervisor 状态机尚未成为生产 task owner | 旧 generation 的 completion 不能释放新代；取消请求不等于线程退出；等待超时保持 retiring，不能因返回超时而并行占卡；Join 需覆盖 IMS/security/IKE/transport 子任务 | E3 / H0；V1、V4 |
| RT-C09 传输选择、代理与诊断子任务 | `G/reader_vowifi_transport.go readLoop`；`R/{transport_selection,proxy_probe,system_proxy}.rs` | Rust 已承担部分选择/探测；Go 实际会话传输仍使用其生产适配 | IKE UDP 代理策略与 Telegram HTTP/PAC 独立；探测与 subprocess 有并发/时间/输出预算；取消探测不影响已建立的 session；结果必须绑定请求代际 | E3 / H0；V1、V4 |

## D. N待调度与多 Profile

| ID / 能力 | 真实入口及触发 | 当前 owner / Rust 基础 | 接管门槛 | 证据 |
| --- | --- | --- | --- | --- |
| RT-D01 inventory、desired 与启动操作 | `G/multistandby_manager.go:81 inventory`、`:299 acquire`、`:380 start`；`G/multistandby_api.go:187` 后台 operation；`R/multistandby_operations.rs` | Rust持久拥有请求指纹、幂等、单次执行claim、操作状态与重启unknown隔离；Go不再有进程内重复执行表，但仍枚举Profiles并执行真实会话/物理卡生命周期 | inventory完整分母不可截取为迁移完成；Rust先耐久接纳再有界认证派发；原子pending→running claim最多一名执行者；未知结果不得自动重放；desired顺序、批次部分成功和同卡身份恢复仍需完整接管 | E7、E5 / H0；Iteration 166、167；V2、V4 |
| RT-D02 卡串行、网络并存 | `G/multistandby_card.go:210 withCard`、`:261 withProfile`、`:385 authenticate`；`B/multistandby` scheduler | Go broker 串行切 profile 与认证；每 profile 网络会话可同时存在；Rust 没有在线等价 N待 broker | 每物理卡一个 actor，switch → settle → 核 EID/ICCID/IMSI → AKA 串行；不要把整个在线网络生命周期锁在卡上；没有“同时物理启用所有 profile”的错误承诺 | E7 / H0；V2、V4 |
| RT-D03 每 Profile 网络 worker 与启动配额 | `G/multistandby_manager.go:480 run`、`:683 acquireStartup`；`R/multistandby_retry.rs` durable retry/REGISTER dispatcher | Go 仍有每 Profile worker、物理 session 运行与串行启动门控；Rust runtime journal 持有 stable-ready clock、observation-gap 判定、单调 Profile generation、attempt-token fence 与 retry/REGISTER durable deadlines；Go 仅提交 Profile/reason 与运行事实并消费匹配 deadline callback，不分配 generation、不计时，也不在 Rust RPC 失败时本地重连 | profile key + generation隔离计时器/SIP/SA；并发启动有界；一张卡失败不停止健康卡；同卡多个在线会话恢复时按卡排队；停止或退役旧代次后迟到回调不启动session | E7、E3 / Iteration173–175及351–354离线验证；H0；V4设备/长稳验收未完成 |
| RT-D04 后台出站短信 flush 与队列准入 | `R/multistandby_maintenance.rs` 1s tick → authenticated `G/multistandby_api.go` adapter；`G/multistandby_manager.go` maintenance channel → `flush` → authenticated Rust `claim-next` → IMS send → Rust outcome；`R/multistandby_send.rs` 处理公开 `POST /api/multistandby/messages/send` | Rust持有全局维护节拍、公开校验、耐久幂等队列接纳、发送 readiness 派生及按现有 message-ID 顺序原子选择/claim 下一项；Go仍负责 flush 生命周期和当前会话的网络发送，Rust记录 accepted/unknown | 每次网络提交前必须 durable claim；同一 attempt 仅授权一次物理发送；结果未知保留 unknown，不自动重放；空队列 poll 不写 journal；重放须匹配相同 durable desired revision；Rust readiness 不替代硬件提交时的 session/generation fence；事务 scope 不使用 RP/TP 8-bit reference 单独识别 | E5、E7 / Iteration 172/192/355 离线集成；无真实短信；V3、V4 |
| RT-D05 自动恢复、状态记录与租期统计 | `R/multistandby_recovery.rs`（5秒 Rust startup scheduler）；`G/multistandby_api.go:multiRuntimeSnapshot` authenticated inventory/session-start adapter；`G/multistandby_runtime.go` 仅提交会话事实 | Rust持有durable runtime journal与逐Profile恢复状态，完成startup polling/reconciliation、原因归一、stable-ready五分钟清零、在线起点与观测时间戳、超过60秒观察缺口判断、人工重试/计划续期/意外失败策略；旧journal没有可证明ready_since时保守清零。Go仍执行硬件盘点、session启动及session retire/reconnect动作，无本地恢复计时器或时长计算 | 无desired intent时不得读卡；区分planned renewal与unexpected failure；保留观测缺口，不把未知时段算在线；Rust scheduler只负责startup reconciliation和deadline触发，不代替Go物理恢复；session退役/执行及实机仍待验收 | E7 / Iteration168–175、351–354；V4、V6；H0 |
| RT-D06 拔卡、停止、恢复原 profile | `G/multistandby_manager.go:748 stopWithDesired`、`:798` 原 profile 恢复、`:822 releaseOwner`、`:925 invalidateReaderGeneration` | Go 收尾；Rust存储 owner/token 提供 fencing | stop 先持久 desired，再等所有 profile worker 实际结束，再恢复原 profile 并释放卡；拔卡不能操作新插入的卡；恢复失败独立报告 | E7、E5 / H0；V2、V4 |

Iteration 168新增 `POST /api/internal/multistandby/runtime/recovery-plan`，Rust持有严格输入约束的恢复策略决策与durable runtime journal；Iteration 169将 Go/Rust DTO 对齐并持久化每Profile退避状态；Iteration 170删除Go启动恢复ticker，由Rust按5秒间隔、durable keep_online意图和Go就绪代次执行一次性启动协调，通过原有认证runtime-snapshot适配器请求Go inventory；Iteration 171把Go传入的运行时观察改为受限事实（tunnel、IMS、messaging、SIM短信路由），由Rust归一化原因并推导报告状态和恢复标记，落盘格式不变；Iteration 172删除Go逐Profile一秒ticker，由Rust持有全局维护节拍和认证回调，Go session worker只接收合并信号；公开短信入队也由Rust校验并耐久提交，Go保留只读就绪查询。Go仍采集物理恢复与IMS/隧道就绪事实并执行session/timer、flush及物理发信，IPC失败时暂保留受限兼容路径。28项监督进程跨语言检查通过；该迁移不是Rust N待session/broker或完整runtime接管。

## E. 短信接收、发送与持久化

| ID / 能力 | 真实入口及触发 | 当前 owner / Rust 基础 | 接管门槛 | 证据 |
| --- | --- | --- | --- | --- |
| RT-E01 模块短信轮询、URC 与删除确认 | `R/modem_actor.rs frame_input`；`R/modem_actor/sms_receive.rs`；`R/usb_sms_receive.rs`；`R/usb_at.rs snapshot` | Rust观察CMTI、维护串口64条有界队列/worker、解码/归档和验证后ME删除；USB独立worker也直接使用Rust真实句柄完成CMGF及SM/ME采集，不再回调Go raw snapshot。Go仅供配置与设备/RF/AKA/eSIM等业务协调 | USB不自动删来源；session不等于SIM身份。读卡器/IMS生产者及硬件业务还在Go；实机插拔与长稳未验收 | E6、E5 / Iteration093原生采集整合/部分断线/事务互斥，Iteration092预览归档，Iteration089/090串口离线通过；H0；V3、V7 |
| RT-E02 IMS 入站持久化与 RP-ACK | `I/sms_raw.go:13 handleIncomingRawSMS`、`:78` 持久化错误返回；`G/multistandby_manager.go:879 inbound`；`G/main.go:801 archiveSMSChecked` → `R/message_archive.rs handle_internal` | Go仍持有IMS接收与ACK时序及读卡器采集；生产持久化RPC交Rust archive writer。Iteration297移除完整易变Go inbox快照发布/接收桥；Rust GET /api/sms 合并耐久归档与Rust USB预览，已归档短信不依赖Go重复缓存；standalone Go fixtures保留本地writer fallback | durable store 成功才 RP-ACK；SIP 200 不是持久确认；损坏/锁存返回失败不 ACK；相同网络重传去重，但相同文本新短信保留；按 profile + SIP transaction + RPDU hash 绑定；跨进程超时以同一内容安全重试 | E5、E3、E7 / 离线RPC与Rust归档回归；实机仍H0；V3 |
| RT-E03 长短信重组、状态报告与缓存清理 | `R/usb_sms_receive.rs project_snapshot`；`R/modem_actor/sms_ingest.rs`；`C/proletarius-sms/src/{reassembly,status_report,udh}.rs`；`I/sms_raw.go` | Rust负责USB SM/ME分别解码与不完整分片保护、串口快照解码和重组；IMS/读卡器仍有Go事件生产及缓存生命周期，不能将纯编解码复用视为完整IMS接管 | reference冲突、out-of-order、重复分段和不同来源隔离；USB不跨SM/ME拼接，损坏PDU保留原诊断。TTL清理不改已持久原始证据；status report不当新入站内容转发 | E5、E6 / Iteration092 USB离线投影与故障验证；H0；V3 |
| RT-E04 设备发信与提交账本 | `R/device_sms_send.rs`；`R/device_send_store.rs:127 recover`；`R/usb_at.rs submit_active_sms`；`R/module_radio.rs submit_usb_device_sms/submit_device_sms`；`R/sms_codec.rs POST /api/v2/sms/prepare-tpdu`；`G/device_sms_route_bridge.go` | Rust拥有durable prepare/claim/finish/archive；普通USB与serial modem路由在Go仅提供初始物理观察后，Rust绑定精确SIM代次并负责本地有效性检查、AT/CMGS提交、reservation及终态落账。USB capability绑定owner session+SIM身份代次，serial绑定modem SIM代次。Go不再 validate/execute 这两种Rust原生binding；Go仍提供初始 capture 和 IMS/reader/legacy兼容路径 | accepted/submitting/unknown语义保持；断线/超时/finish丢失不自动重发；owner task等待真实物理I/O结束后再释放reservation并确认账本；发送成功与写档失败分别记录；初始capture及IMS/reader/legacy路径、QMI/MBIM生产编码仍待迁移；明确区分离线合成与实机发信 | E5/E6 / Iteration349/350/467 Rust USB/modem owner与账本回归、全crate离线测试；无实机；V3 |
| RT-E05 N待/device store 恢复与故障隔离 | `R/main.rs initialize_sms_stores`；`R/device_send_store.rs:119 open`；`R/multistandby_store.rs:265 open`；`R/sms_store_health.rs:34 GuardedArchive` | Rust 已在线；启动和 Go 重启触发 recover；故障锁存到重新启动 | 损坏、unlink、截断、非法 schema 不覆盖为空；第一错误 truthful 503；坏库不影响另一健康库；恢复不能把 submitting 变成可自动重发 | E5 / H0；V3、V6 |
| RT-E06 短信/通话 archive 与文件锁 | `R/message_archive.rs`；`R/call_archive.rs`；`R/call_facts.rs`；`R/modem_actor/sms_ingest.rs`；`R/usb_sms_receive.rs` | SMS耐久归档和列表合并由Rust处理，串口及USB接收worker直接调用archive；新增一次耐久精确ID批量删除，USB采集/归档/预览与物理变更共享生命周期锁，旧操作不清新会话预览。Iteration297后公开列表只合并归档和Rust USB预览；Go重复缓存仅接收稳定ID本地修剪，不再跨进程传送完整快照。三来源通话事实、冻结终态及耐久writer也归Rust；HTTP取消不取消已开始的写入 | USB实体owner已迁Rust；Go短信接收/IMS生产、配置/业务协调和物理存储工作流仍待退出；已开始历史写入不回滚，但旧producer/清理前轮次不可污染新预览。通话未确认事实跨Rust崩溃恢复、升级/回退及实机尚缺 | E5、Iteration091/092离线及race；H0；V6 |
| RT-E07 导出快照、清理器与 I/O 配额 | `R/sms_export.rs Exports`、`:206` cleanup task、`prepare_guarded/pin_guarded`；`R/archive_io.rs` | Rust 已原生导出和临时快照回收；P1 bounded worker 是基础加固 | 对同一受保护 FD 流式解析；源读/parse失败锁存，输出/预算错误不误锁；取消/过期释放快照；导出不能挤死内部持久化收尾 | E5 / H0；V1、V6 |
| RT-E08 USB AT SIM/ME 短信存储与快照 | `R/usb_at.rs snapshot/read_snapshot`；`R/usb_at_storage.rs`；`R/usb_sms_storage_service.rs`；`G/usb_sms_storage_api.go`公开兼容入口 | Rust拥有CMGF/CPMS/CMGL快照及SM/ME解码/长短信处理，也拥有usage/clear/delete/delete_sender物理计划。删除凭据绑定ICCID/session/全部PDU来源，逐项复读并要求完整CMGD确认；恢复原选择，未知结果不重放。Iteration197–199后Rust拥有公开清空、单条与按发送方删除策略和归档清理；Iteration297后Go仅按稳定ID修剪重复内存缓存，不再向Rust同步整份缓存。Iteration341后，公开单条删除直接由Rust校验当前收件快照凭据，并在Rust USB AT owner内独占执行卡身份/PDU复核、CMGD与CPMS恢复；Iteration342后，按发送方删除也按当前完整proof registry选取精确来源并在同一Rust owner执行；只在设备确认后清理耐久归档并让Go修剪重复预览。Iteration183后，公开GET用量也由Rust gateway直达同一实体owner，模块通道忙时拒绝竞态查询；Go仍保留SMS清空业务适配、初始短信发送路由观察、IMS与standalone兼容入口。Go物理状态采集和standalone兼容handler仍在 | 缺卡身份不授权删除，UI预览上限不当作全部来源；历史ID碰撞算法未重写；真实CPMS/QCCID兼容、插拔/阻塞与实体删除仍待验收，Go公开适配仍未退出 | E6 / Iteration095物理/服务故障、生命周期与Go race；Iteration093采集；无实机；V2、V3、V6 |

## F. 短信转发准入、队列与平台凭据

| ID / 能力 | 真实入口及触发 | 当前 owner / Rust 基础 | 接管门槛 | 证据 |
| --- | --- | --- | --- | --- |
| RT-F01 设备短信转发准入与首次基线 | `R/device_sms_forwarding.rs` 后台合并Rust持久短信归档和Rust USB有界预览，生成兼容通知ID并分批写入耐久队列；`R/sms_forwarding.rs::device_snapshot`、`R/device_forwarding_store.rs`；Swift只提交一次旧检查点handoff | 实时准入与转发producer由Rust后台运行，不再轮询Go `/api/sms`或读取Go volatile inbox副本；Go仍可能触发模块/IMS短信事件进入Rust归档，并保留UI兼容快照来源 | Rust producer首次基线、启停代次、digest/ledger及批次提交已覆盖；跨进程强杀窗口确认未知结果不会自动重放，旧空账本可恢复；Iteration165跨进程验证N待IMS入站和Go设备归档两条事件入口均自动投递一次且不重复；仍需更广旧格式/损坏/存储故障矩阵、外网provider与真实设备验收；关闭期历史不能回填 | E8、E10 / 离线；实机仍H0；V5 |
| RT-F02 N待转发 changes 游标 | `S/MultiStandbyStore.swift` 的 `forwardingAdmissionSnapshot` 只读旧 `UserDefaults` 检查点；`R/forwarding_admission.rs`、`R/forwarding_admission_store.rs` | 当前 Swift producer已移除。Rust producer在durable handoff后读取Rust changes feed；成功durable入队后推进独立游标。注入测试覆盖未知队列结果、游标提交失败重试、禁用/重开高水位和handoff冲突失败关闭；SWF1文件after-rename不确定故障已有测试，完整fsync/旧格式/崩溃矩阵仍待 | handoff不可重复改变；不能混用通知游标；配置代次变化不得错投；durable ack先于游标推进；重启不漏转也不重放历史 | E8、E10 / H0；V5 |
| RT-F03 转发 worker 与后台 flush | `R/sms_forwarding.rs` 配置worker与 `C/proletarius-forwarding/src/worker.rs` 队列执行器；旧 Go controller 已退役 | Swift转发API经Rust daemon认证分派，Rust Service持有配置代次、SWF1排他锁、后台flush worker和退出取消；仓库没有 Go 生产引用，仅留 Go/Rust flock 测试peer | 每配置代际唯一owner；切换/退出先停止旧代；网络结果未知不自动重试；本地队列冻结分片/目标兼容；Rust进程生命周期与App页面脱钩；Rust阻塞provider期间队列仍响应且不会重复提交 | E10 / H0；V5 |
| RT-F04 Telegram / Bark 网络投递 | `C/proletarius-forwarding/src/provider.rs` Rust provider 与 `sms_forwarding` worker；旧 Go provider 已退役 | Rust实现代理、Bark图标/加密、业务状态校验和Telegram分段checkpoint；Iteration458 在 macOS 执行 CFNetwork 内存合成 PAC 脚本测试（6/6 system-proxy测试通过）及 forwarding 56 passed；本地mock/夹具，不代表真实系统代理或provider外网送达 | 指定测试端点后核验Telegram系统代理含真实PAC URL/自定义代理、provider body业务校验和分段checkpoint；Bark host/AES-CBC/PKCS7兼容；HTTP200不等于provider成功；脱敏错误，禁止自动重复未知投递 | E10、Iteration458 / H0；V5 |
| RT-F05 队列加密、凭据与配置同步 | `S/SMSForwardingStore.swift`配置/Keychain；`C/proletarius-forwarding/src/{lib,queue_store}.rs` SWF1实现；静态 `go-state.*` 是历史兼容fixture | Swift从Keychain提交配置和队列密钥至本机认证Rust endpoint；Rust覆盖共享锁、加密原子写、迁移队列、unknown gate和结果提交；Rust测试消费Go生成的固定SWF1 golden；Go只保留flock互操作测试peer。未读取真实用户队列、Keychain或历史文件 | AES-GCM SWF1、nonce/AAD/HMAC、冻结Telegram分段逐字兼容；扩展历史格式/损坏矩阵；钥匙缺失/损坏fail closed；凭据仅走本机认证路径；拆分状态密钥和配置secret的最小暴露范围 | E10 / H0；V5、V6 |

## G. 语音与音频

| ID / 能力 | 真实入口及触发 | 当前 owner / Rust 基础 | 接管门槛 | 证据 |
| --- | --- | --- | --- | --- |
| RT-G01 完整 SIP 呼叫控制 | `I/call_runtime.go:302 watchInvite`、`:618 handleRequest`、`:765 handleInvite`、`:884 answer`、`:1055 hangup` | Rust `IncomingCallTracker` 拥有来电接纳/重传、Trying/Ringing/Connecting/Active/Ended phase、原 INVITE Call-ID/CSeq 的入站 CANCEL 接受/拒绝决策，以及匹配 CSeq 且仍 Ringing 的超时裁决、受跟踪来电的 BYE 终止准入、来电本地挂断 reject/BYE/ignore 裁决与本地 BYE CSeq 分配；入站 PRACK/UPDATE 的 SDP 候选复用 Rust negotiator，但媒体重协商尚未启用；Rust `OutgoingCallTracker` 处理外呼 INVITE 策略。Go 仍实现 SIP sockets/responder、PRACK/ACK/CANCEL/BYE 写入、SDP transport adapters、call lifecycle、硬件 readiness、SIM/USB锁、媒体与通话记录 | SIP dialog lifecycle、超时调度、跨 INVITE/BYE 事务关联、物理/媒体副作用与 Go owner 仍待退出；实机呼入、呼出、取消/应答竞争、音频验收缺失；N待 SMS-only 不创建 call runtime | E3、E11 / Iteration 393–396 离线；H0；V7 |
| RT-G02 RTP、编解码与 DTMF | `I/call_runtime.go:1173 applySDP`、`:1384 startMedia`、`:1445 DTMF`、`:1532 uplink`、`:1591 downlink`；Rust `C/proletarius-call/src/{lib,evs}.rs` | Go仍拥有生产live media、codec negotiation/probe与RTP worker；主机策略EVS→AMR-WB、无PCMA，尚未完成生产接管。Rust有受限unsafe EVS Primary WB 13.2/AMR-WB native wrapper、EVS compact/full ToC packetizer/depacketizer及3GPP离线黄金向量；Iteration302新增有界RTP重排窗口（序号回绕、重复/迟到包、SSRC重启、32包限额、40ms缺口推进）。媒体任务、SDP codec结果和RTP socket仍由Go live worker使用；公开DTMF准入已由Rust持有，媒体内DTMF/RTP发送仍由Go执行 | 将Rust SDP codec结果及reorder接入真实call actor；迁移媒体任务生命周期、RTP收发/jitter/序列/丢包恢复、DTMF发送、Audio bridge与退出回收；Go owner退役后做独立全语音 parity 和来去电双向音频验收；单元/黄金向量不等于通话验收 | E11 / Iteration301 codec与frame、Iteration302 reorder离线测试；H0；V7 |
| RT-G03 macOS CoreAudio桥 | `S/ReaderVoWiFiAudioBridge.swift:62 start`、`:247 stop`、`:377 receiveNext`、`:547 recoverFromAudioRouteChange`；`S/CallAudioStore.swift` | Swift CoreAudio平台实现，终态允许保留；通话决策归Rust | PCM桥鉴权、bounded缓冲与音频设备切换保留；旧音频回调不能污染新call；业务ringback/mute/route状态接口明确，底层音频线程停完才释放 | E11 / H0；V7 |
| RT-G04 模块 VoLTE / USB audio / helper | `R/call_mode_lifecycle.rs`；`R/call_mode_state.rs`；`R/module_radio/call_mode.rs`；`R/call_mode_status.rs`；`R/call_mode_runtime_download.rs`；`R/call_mode_adb.rs`；`G/call_mode_lifecycle_client.go`；`G/module_voice_darwin.go` | Iteration479：单一 Rust 状态/worker 接管 enable 备份、授权、USB/IMS/VoLTE 固定事务、重启后同模块重绑及初始化、restore live binding 验证/写入、retry/refresh、下载进度与验证后部署；公开 flags/disclosure/last_restore 保留。Native admission/read/physical worker 贯穿真实 release/USB 清理，producer 退休和 shutdown 不提前复用。Go 状态/cache/generation/worker、备份/投影/下载与 retry adapters 已删除；只留 native authority 客户端、ordinary stop、只读类型/path helpers 和 transport lock | 完整来去电/PCM、helper 校验、USB 配置备份恢复仍需设备验收；QMI/MBIM、不支持平台及 Go IMS 会话执行不因此退出 | E6、E11 / Iteration479 native lifecycle/状态/固定动作、session-fenced close 源码审阅与合成回归；历史 Iteration093、122、134–138 夹具；H0；V7 |
| RT-G05 通话状态发布与记录收尾 | `R/module_call_state.rs`；`R/call_facts/native.rs`；`R/call_facts.rs`；`G/call_facts_bridge.go`；`G/call_history_error_client.go`；`I/call_observer.go` | Iteration480：原生 module_cs 独立 producer、唯一状态/revision/pending dial 与有界事实队列；原始 serial/USB URC 归原生 capture，Go frame 标志抑制双消费。Rust worker 保留冻结队头直到真实耐久确认，物理预约释放后才 retire/drain。Go 仅保留 module_ims/reader_ims 有序事实、owner/epoch、ACK 重试和两来源历史错误发布；Go producer 替换不能清原生 CS，私有 observe 拒绝 CS | 两 IMS session/SIP/媒体仍在 Go；native aggregate 错误投影不反查 Go CS，错误恢复仅跟实际 ACK；异常强杀与实机存储失败/通话未验收，未确认 release 不复用 authority | E6、E11 / Iteration480 native CS state/facts、Go 完整 test/build 与定向 race、IMS 回复投影 5 项；H0；V6、V7 |

| RT-G06 完整模块蜂窝 CS 控制 | `R/module_call.rs`；`R/module_radio/call.rs`；`R/module_radio_transport.rs`；`R/modem_actor.rs`；`R/usb_at{,_transport}.rs`；公开 `/api/call/{status,dial,answer,hangup,dtmf}` | Iteration480：Rust retained job/physical lane 接管新请求准入、两次状态与 readiness 验证、实际串口/USB 写前 revision/session guard、固定 CLCC/ATD/ATA/ATH/VTS、期望 RF 预约 worker、挂断后的既有 audio stop policy。Go 五私有 adapters、四重复公开 routes、CS projection/worker/callback/source 均退役；Go 公共 SIM/USB 锁继续服务其他业务 | cancellation 不取消已接纳物理任务，未知提交不 replay，未确认 release 不复用 token；新 ring/CONNECT/stale poll 竞争和旧 session 不能影响新 authority；shutdown 须等待真实 worker/物理释放再 retire/source drain。真实 modem/音频/运营商与整体零 Go 启动仍待验收 | E6、E11 / Iteration480 native fake transports/CS state/service，定向 race 与完整 Go 回归；H0；V6、V7 |

## H. 辅助定时器、日志与安全状态

| ID / 能力 | 真实入口及触发 | 当前 owner / Rust 基础 | 接管门槛 | 证据 |
| --- | --- | --- | --- | --- |
| RT-H01 EAP exchange 生命周期 | `R/eap_identity.rs:82 take`、`:103 release`、`:114 prune`；`R/main.rs` 30s cleanup tick | Rust在线，Go IKE在退出时回调release | 绑定reader/application/profile generation；consume/release/TTL幂等；未完成交换限额；进程退出不会把旧身份复用给新卡 | E3 / H0；V1、V2 |
| RT-H02 安全诊断、统计与日志 writer | `B/safeevents/events.go:99 Begin`、`:113 Record`、`:132 append`；`G/multistandby_runtime.go:312 saveLocked`；`S/BackendProcess.swift:316 drain`；`S/BluetoothReaderManager.swift:122 bleDiagLog` | Go安全事件/租期journal、Swift日志桥；Rust有局部统计/错误码但无整体替代owner | 有界队列/文件/保留期；只输出固定事件枚举与脱敏身份；不记SMS正文/token/CK/IK/下载码；未知观测区间保留；日志故障不阻塞通信退出 | E7；`B/safeevents/events_test.go` / H0；V1、V6 |
| RT-H03 配置文件与缓存的 writer 总账 | `C/proletarius-vowifi/src/carrier_overrides.rs`；RT-B12、RT-D05、RT-E05/06、RT-F05、RT-G04中所有writer | 多种文件格式已存在，不能以迁移单数据库替代；Rust已接管部分 | P0给每个现有文件建立path来源/schema/owner/权限/恢复策略；移除旧writer前验证import/export与回退；运行缓存不当权威身份；不读取生产目录填fixture | E5、E10；V6 / H0 |

## P1 actor 接管依赖与禁止跨越的边界

| 顺序 | 需要先成立的能力 | 对应清单 | 允许并行的工作 / 不允许的捷径 |
| --- | --- | --- | --- |
| 1 | 可取消且有预算的后台任务、操作ID、代际、日志、存储故障状态 | A01–A05、E05、H01–H03 | 可以先做离线限流/状态机；不把HTTP超时当物理任务已取消 |
| 2 | 每物理设备唯一actor、APDU队列、真实退出确认 | B01–B04、C08 | PC/SC先闭环；BLE/模块另列平台适配。不得在Go仍占有卡时启动Rust第二owner |
| 3 | 单profile完整短信runtime | C01–C06、E02–E05 | 用真实已有Rust库组成event loop；实现业务事务分派和durable ACK。仅REGISTER成功不满足此阶段 |
| 4 | 租期/断网/唤醒、退休代排空与N待 | C04、C07–C08、D01–D06 | 卡操作串行、网络会话并行；停止全部相关子任务后才释放physical owner；同会话REGISTER优化不列前置依赖 |
| 5 | 全语音能力与模块语音 | G01–G05、B07–B10 | SMS-only N待可以先验收；全后端去Go必须等待voice，不能删除原voice功能 |
| 并行 | 正常LPA、转发、模块控制与持久格式兼容 | B07–B12、F01–F05、H03 | 可共享actor/事件契约先写离线逻辑，生产接入仍须唯一owner；不改6982特殊认证 |
| 收尾 | Go producer、writer、timer、构建依赖全移除 | 全部 | 逐行记录生产切换及移除证据；升级/回退迁移验证后才最终打包 |

建议最小事件身份：`backend_instance + physical_device_id + card_epoch + profile_id + session_generation + operation_id`；SIP事务另含 `Call-ID/CSeq/method`，发送记录另有持久 `request_id/attempt_token`。设备身份不得只用读卡器展示名；业务ID与日志脱敏ID分离。

事件排空顺序必须是：停止准入 → 标记retiring并取消定时器 → 停业务发送 → IMS reader/writer/listener及security transport退出 → IKE/ESP RX/TX退出 → 阻塞卡调用实际返回 → 必要恢复操作完成 → durable finish/退出状态 → 释放owner。等待超时只改变可观察状态，不允许重用硬件。父actor须知道所有子任务，不能依赖对象Drop间接推断线程已结束。

## 网关请求、头超时和阻塞预算的边界

Iteration097：`R/public_http.rs`在既有鉴权后对明确公开GET提供HEAD，同一路由只执行一次并保留表示长度；21条旧Go派生路径恢复405/Allow，纯native错误与Go专属HEAD原行为保留。真实UDS覆盖内部权限/退出/导出，独立Go→Rust wire差分通过；URL清理/转义/尾斜线未验收。此项不代表新业务owner或Go退出。

当前 P1 最小改动的范围：`R/http_server.rs` 共用 HTTP/1 builder，5s header timeout，正常/退出两种服务均使用；`R/blocking_pool.rs` 先异步申请 semaphore，permit随真正blocking closure持有，caller drop只能取消尚未开始工作；`R/archive_io.rs` 配额8。PC/SC inventory扫描由 `R/reader_inventory.rs scan` 与 `R/snapshot.rs capture` 两个入口共用配额1。**这些配额不是全局请求/排队长度限制，也不是actor接管完成。**

独立只读复核未发现新blocking pool取消语义或header timeout作用域的阻断问题；相关测试由实现者执行，本审阅未重跑。最初 `snapshot.rs capture` 直接 `spawn_blocking` 绕过inventory pool的问题已反馈，实现者修改后已重新只读核对，两条入口现共用 `reader_inventory::scan`，此项关闭。

后续准入限制要独立设计，不能给所有路由简单套一把semaphore：

1. 分开“已接受但头未完成的连接数”“正在执行的公共请求数”“认证内部回调/收尾数”“实际blocking worker数”和“actor mailbox长度”。每个计数分别有上限、排队策略和指标。
2. Go收到公共发信/启动请求后会回调Rust做claim/inbound/finish等持久操作。若公共请求持有全部名额且等Go，内部回调再争同一名额，会发生等待环。内部持久化及reservation/EAP release需保留容量；退出时也保留cleanup-only准入。认证和body上限必须早于昂贵任务。
3. header timeout只覆盖头部接收；body大小/读取停滞另设预算；不要以统一5s deadline取消正在写卡、提交短信、队列checkpoint或模块恢复。返回已接纳operation ID，明确queued/running/submitting/unknown/finished语义。
4. blocking permit在实际closure内释放，async waiter超时不释放硬件/磁盘实际占额；等待容量时取消应确保不执行后续副作用。已经开始的durable写可能继续完成但响应丢失，调用者必须用token查询结果，不能据超时自动重放。
5. 磁盘、PC/SC枚举、APDU、外部命令、网络provider投递分开预算；公共导出/文件锁拥堵不能饿死内部finish。当前archive pool仅限制实际并发；后续按存储队列优先级证明内部收尾可进展，再决定是否分池，避免为了限流制造更多锁顺序。
6. 新请求拒绝需区别“尚未提交，可安全重试”和“可能已提交，不可自动重试”。状态查询也需有限的独立容量；不能以健康接口可达冒充通信runtime健康。
7. SIP/RTP/ESP事件不通过HTTP request semaphore排队；它们使用自己actor的有界队列、deadline与取消策略。不能因导出或UI轮询拥堵导致保活饿死。

下一包的候选预算可从 public 64 / authenticated internal 32 / cleanup 16 与总连接256开始做压力夹具，均为待测参数而非本轮生产改动。超额采用非等待拒绝，认证和分类必须在取得对应lane前正确完成；POST不能因为收到503就推断先前尝试可重放。单一UDS仍可能被尚未认证的连接占满，5s头期限只能限制每条连接占用时间，不能保证持续慢头流量下内部回调有进展；这个限制需明确记录，并用隔离内部listener或预留连接入口解决后才承诺强隔离。此次不为解决该问题扩大生产改动范围。

## 现有离线证据入口

下表是可复用夹具的位置，不宣称本轮执行通过，也不表示覆盖了相应能力的全部语义。

| 编号 | 现有源码入口 |
| --- | --- |
| E1 进程/退出/请求 | `G/communication_shutdown_test.go`；`R/main.rs` tests；`R/gateway.rs` tests；`R/http_server.rs` tests；`R/blocking_pool.rs` tests |
| E2 读卡器/lease | `R/reader_task.rs` tests（queued取消与运行锁保持）；`R/reader_reservation.rs` tests；`R/reader_session_lease.rs` tests；`C/proletarius-reader/src/session.rs` tests；`G/reader_vowifi_eap_session_test.go`、`reader_vowifi_eap_session_e2e_test.go` |
| E3 IMS/session | `G/reader_vowifi_session_test.go`、`native_vowifi_session_policy_test.go`；`R/native_vowifi_supervisor.rs`、`native_vowifi_session.rs`、`eap_identity.rs` tests；`C/proletarius-vowifi/src/{registration_coordinator,security_runtime,startup_loopback}.rs` tests；`B/vowifi/imscore` 包测试 |
| E4 ESP/IKE | `C/proletarius-ike/src/{esp,sk,keymat}.rs` tests；`C/proletarius-ims/src/security_esp.rs` tests；`backend/third_party/swu-go/pkg/{ipsec,crypto,swu}` 包测试，含ESP序列耗尽/RX退出 |
| E5 存储/SMS | `R/{device_send_store,multistandby_store,sms_store_health,sms_export}.rs` tests；`C/proletarius-store/src/lib.rs` tests；`C/proletarius-sms/src` 各codec/reassembly tests；`app/Tests/SMSStoreHealth/main.swift` |
| E6 模块/AT | `B/modem/manager_force_release_test.go`；`B/modem/sms_receive_test.go`；`G/module_flight_mode_test.go`；`B/backend` 包测试。与实际macOS/libusb/QMI设备支持仍须逐项映射 |
| E7 N待 | `G/multistandby_*_test.go`；`B/multistandby` 包测试；`R/multistandby_store.rs` tests。未来Rust调度器必须重新执行同义故障场景 |
| E8 Swift准入/轮询 | `app/Tests/SMSStore/main.swift`；`app/Tests/MultiStandby/main.swift`；`app/Tests/SMSAttention/main.swift`；`app/Tests/DeviceSMSSend/main.swift` |
| E9 BLE | `app/Tests/BLEManager/main.swift`；`app/Tests/BLEPhaseDeadline/main.swift`；`app/Tests/ReaderBLESelection/main.swift`。mock回调不等于实机GATT断连恢复 |
| E10 转发 | `backend/internal/forwardinglockcompat/lock_interop_test.go`（仅双向 flock peer）；静态Go SWF1 golden；`app/Tests/{SMSForwardingClient,SMSStore}/`；`R/{forwarding_handoff_store,device_forwarding_store,sms_forwarding,device_sms_forwarding}.rs`及路由审计；`C/proletarius-api-contract`；`C/proletarius-forwarding/src/{lib,queue_store,tests}.rs`。Go第二执行器与其专属行为测试已退役；Rust拥有worker、SWF1队列/provider路径、设备短信与N待归档producer。设备短信实时列表读取与准入由Rust后台触发；Swift仅保留配置/凭据同步及一次旧准入handoff，Go仍有部分IMS/模块归档事件producer。21项Rust/Go隔离进程集成覆盖合成本机provider强杀/重启/未知结果防重放/显式重试，以及N待IMS事件和Go设备归档事件各自自动投递一次；更广旧格式与持久化故障矩阵、系统PAC URL集成和指定端点真实投递仍待验收 |
| E11 语音 | `B/vowifi/imscore` call/media测试；`C/proletarius-call/src/{lib,amr,pcma,sdp}.rs` tests；现有module voice/call mode夹具。codec单测不能证明完整通话 |

## 下一阶段需要落地的离线与实机验收

| 编号 | 离线必须证明 | 实机核验要求 |
| --- | --- | --- |
| V1 任务与预算 | 半头连接到期不dispatch；完整头后长任务不中止；容量等待取消无副作用；已运行任务取消仍占额；worker panic释放预算；公共64与计算回调32慢头占满时，超额连接等待且 Go 仍能通过独立账本16路完成真实跨进程持久写入；退出 finish/cleanup；队列容量与拒绝语义 | 观察真实负载CPU/内存/线程/FD/队列高水位，不用模拟时钟冒充实机运行 |
| V2 卡actor | 换卡/拔卡/同名reader重连/旧回包；APDU取消后原worker仍占卡；两个设备互不阻塞；Go/Rust双owner拒绝；身份校验与复位失败可见；logical channel完整关闭 | PC/SC先，随后BLE与实际模块；必须指定card/reader，写卡/切卡需要真实目标；本轮无设备操作 |
| V3 短信闭环 | SIP拆包/粘包/乱序、NOTIFY/MESSAGE交错、PortS响应路径；持久失败不RP-ACK；同文本不同事件不丢；claim后崩溃/响应丢失不重复发；unknown不重放；状态报告不作为普通短信转发 | 单profile入站/出站/重传与存储故障；仅在用户授权目标上执行真实短信。协议fixtures与真实运营商证据分别归档 |
| V4 生命周期/N待 | 旧generation late completion、停止与重连竞态；所有Rx/Tx/join排空；自然租期原因模型与虚拟时钟分别测试；多profile切卡串行与在线网络并行；恢复原profile失败处理；休眠/断网/切代理恢复 | 阶段连续24h；终态至少72h且至少3次自然租期事件。按IMS续期、IKE/CHILD rekey、reauth分类，未发生的类别标未覆盖，不用缩短timer补通过；所有profile为最终分母 |
| V5 转发接管 | Swift→Rust一次性handoff崩溃矩阵；首次启用不回放；换配置不送错目标；关闭UI继续入队；SWF1坏key/截断不重置；Telegram冻结分片/unknown与Bark非200业务码；系统/自定义代理夹具 | 指定测试接收端后验证Telegram/Bark、图标/加密/代理；不能借迁移测试发送真实历史短信 |
| V6 数据和writer | 每份文件唯一writer；合法历史schema、未知字段和权限保留；磁盘满/rename失败/锁冲突/损坏/缺失/恢复；导出预算与源损坏区分；回退读取同一兼容数据不重复提交 | 仅用隔离副本或用户授权数据；迁移失败保留原文件，不从新建空库“恢复” |
| V7 完整语音与模块 | SIP完整dialog事务、RTP时序/丢包/DTMF/codec，URC与命令交错；radio/USB/helper恢复、正常LPA确认与取消 | reader VoWiFi与模块VoLTE分别验收来去电、拒接、挂断、双向音频和设备切换；短信阶段不能替代voice阶段 |
| V8 Go移除 | 生产依赖图、构建脚本、发布包、启动链无Go；所有RT项有切换记录；遗留fixtures/许可证保留；无Go隐式fallback | 上述72h与自然事件证据基于不含Go的候选包，不能用混合后端测试结果直接签收 |

每次验收记录源码hash、环境、fixture版本、运行命令、时间、真实/模拟属性及失败项；实机仅记录脱敏卡/设备身份。P0退出要求不是“文档存在”：应逐行确认不存在未归属的producer/timer/writer/exit hook，指定接管阶段，并把遗漏作为新增稳定ID追加。生产接管时应补 owner切换点、旧worker实际结束证据及保留数据的回退方式。


### Iteration 180 能力边界更新

短信设备列表/选择的Rust投影只消费有界Go实时事实；真实reader session和短信硬件执行仍由Go负责。选中BLE reader的LPA下载已进入Rust APDU状态机，Go仍管理跨进程lease、VoWiFi清理、冲突协调与Swift CoreBluetooth transport。IMS OPTIONS heartbeat代次和最终响应关联归Rust，Go仍持有SIP socket与stream。`GET /api/reader/info` 的兼容响应组装归Rust，私有generation-fenced Go adapter仍负责物理reader/USIM观察。Iteration185已将正常监督模式的手动 `/api/at` 公共入口校验与RF策略迁入Rust；Go仍通过认证执行适配器持有SIM/APDU互斥并回调Rust执行。Go显式兼容启动仍保留旧入口。没有进行实卡或运营商验证。

### Iteration 182 能力边界更新

监督模式下，模块 `AT+CLCC` 原始响应的行解析与通话状态聚合现由 Rust modem actor 执行。私有端点要求内部token，字段封闭、请求/原文有界，不执行AT或SIM操作。Go仍持有串口读取、模块SIM事务锁、旧 URC 新来电短期回退、通话状态代次校验及归档观察时序；公开 `/api/call/status` 与拨号/接听/挂断接口仍是 Go-owned。Rust投影端点不可用时只允许使用现有新鲜incoming URC缓存，否则状态返回unknown，不恢复Go parser。该变化仅迁移解析和投影逻辑，不证明Go通话会话或IMS已退出。


### Iteration 183 能力边界更新

Rust gateway现直接处理 `GET /api/sms/storage`，usage查询通过唯一USB AT owner并在活动SIM lease、radio inhibit或在途I/O时拒绝；模块SIM操作忙时返回忙状态，缺少USB设备时返回LOCAL归档用量。原 `/api/esim/operation` GET兼容别名复用Rust observer。Go仍提供设备发现和独立兼容服务；该轮没有修改删除/清空物理操作的owner。

### Iteration 187 能力边界更新

监督模式下 `AT+CLCC` 固定 typed query 由 Rust modem actor 在同一独占串口上执行和解析，3秒命令期限；Go modem adapter 仅验证结构化状态，单次RPC预算8秒。Go继续持有模块SIM互斥、USB transition准入、通话状态观察代次、10秒内新鲜incoming URC兜底和call-facts历史记录调用顺序。兼容启动若没有 Rust typed-query 能力仍走旧 AT 命令和 Rust CLCC parser；公开 `/api/call/status`、通话控制、SIP与媒体未迁移。伪串口/跨语言 adapter离线测试与全量迁移门禁通过，不构成实机通话验收。

### Iteration 188 能力边界更新

监督模式下，模块拨号、接听、挂断与 DTMF 通过固定 `action/value` RPC 请求 Rust modem actor。Rust 校验动作和参数，并在 actor 的独占串口上构造执行对应固定 AT 命令；调用方不能提交任意AT文本。Go仍持有公开呼叫控制路由、SIM互斥、USB transaction准入、呼叫状态与URC观察、通话历史副作用，以及SIP/音频和呼叫生命周期。显式兼容串口保留旧固定命令路径。离线伪串口、RPC认证与迁移门禁通过；没有实机拨号验收，也不代表完整通话后端脱离Go。

### Iteration 189-193 能力边界更新

Iteration189将公开DTMF字符验证与活动呼叫准入决策迁入Rust，但真实呼叫状态仍来自Go、DTMF媒体发送仍由Go执行。Iteration190将选中BLE reader AKA入口派发和卡身份/代次检查纳入Rust；Swift CoreBluetooth transport及Go lease/admission/finalizer仍在。Iteration191将IMS OPTIONS heartbeat的generation-scoped确认/恢复/teardown策略纳入Rust，Go仍持有SIP socket、响应分发、网络I/O、readiness副作用和heartbeat计时器。Iteration192将N待短信发送ready计算纳入Rust，Go仅提供经过认证的原始硬件/session/IMS/SMS事实；Go仍执行flush及物理发送。Iteration193将模块拨号入口号码解析/校验、空闲态准入及冲突响应纳入Rust；Iteration194将接听incoming-state准入和挂断idle no-op迁入Rust。Go通过认证适配器提供实时事实并在实际动作前再次复核状态，保留answer readiness、SIM/USB准入、modem命令、通话事实/历史及挂断音频路由清理；状态观察与动作没有原子revision fence。各切片通过隔离状态/桥接测试和完整离线迁移门禁，未进行实卡、真实运营商或真实呼叫验收。

### Iteration 186 能力边界更新

监督模式的公开 `GET /api/esim` 刷新、错误和缓存回退策略及兼容响应现在由 Rust 处理；Go 的 generation-fenced 私有适配器只提供 eUICC/APDU 观察结果、缓存与底层错误类别事实。Rust 单测覆盖只读 stale snapshot、eUICC unavailable、VoWiFi owner conflict 和操作占用；合成 Go manager 拒绝所有硬件访问。eSIM 下载/确认/切换/删除和实体 APDU 生命周期仍受 Go manager、读卡器会话及实际设备依赖阻断，未进行实机验证。
