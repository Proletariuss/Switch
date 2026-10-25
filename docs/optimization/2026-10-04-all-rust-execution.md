# 全 Rust 后端：可执行 API 契约清单（P0）

> **执行更新（2026-10-25）：** 当前3.51/build351稳定性整改与所有权以[当前进度](all-rust/progress-current.md)及[当前运行时所有权](all-rust/README.md#current-runtime-ownership)为准，来源状态 `source-not-final-G0B`。以下513及后续旧条目的“最新/当前”、接口计数和百分比均是当时历史切点，保留原证据，不作为本轮PASS。G0B/G8/G9仍NO_GO，正式长稳0小时；P0未闭合、P1–P4验收中、P5/P6开放，整体分母未冻结，本轮无DMG。

最新进度记录（Iteration 513，2026-10-16）：本轮完成同机隔离源码重建：Rust/Swift/EVS输出全部重新编译，PATH无Go，签名、动态依赖及包内原生启动/退出通过；旧数据和SWF1队列新增3项真实独立进程恢复测试。完整Rust回归3010通过/0失败/1忽略，构建审计最终自检通过，2853个冻结输入零漂移。修复可选源码快照工具残留的Go版本调用。契约294/294 Rust、127个Swift调用点、429个审阅范围和54条HEAD保持；生产业务代码本轮未变。12项设备/业务验收、完整升级回退、受控性能及自然到期长稳仍待完成。隔离重建不等于全新机器或全功能验收，当前整体百分比不作无依据估算。未生成DMG或替换已安装应用。详见[Iteration 513](../iteration-513-progress.md)。

**进度审计说明：38.31% 为历史估值，阶段账本未闭合，当前精确百分比待重建。** Rust 证书验证已接入 macOS 系统信任和签名校验；软件验证不等于 Three UK 或其他运营商实网验收。该段原有 Go 依赖阻塞为历史状态；当前源码与默认构建链已去除 Go，实机与长稳仍待验收。
### Iteration 468：普通模块短信按当前 transport 在 Rust 捕获 owner

普通 `dji` 模块发送在无 VoWiFi/无显式 subscription 时跳过 Go route capture，由 Rust 读取当前 ModuleRadio transport 并捕获 modem SMS generation 或 USB session/SIM generation。USB transport 不会被残留 serial generation 覆盖；提交继续使用 Rust owner、单次账本 claim、归档及 unknown/no-replay 语义。Reader、显式 subscription、VoWiFi和无可用 Rust owner仍落入既有 Go capture 兼容分支。本切片只证明受限分支，不足以记完整设备短信端到端接管。

Rust daemon 1,473 passed/3 ignored，短信专项25项、Rustfmt、45项合同自测、合同 `--diff` 与差异检查通过；census 354（Rust293/Go61）、127 Swift调用点、379审阅范围。Go退出审计仍6类/266源；Rust-only route gate仍有178条契约依赖Go，项目保持37.99%。没有真实SIM/短信验收。上轮生成的 iteration468 DMG 早于本轮 transport 修正，不含最新版源码。详见[Iteration 468](../../iteration-468-progress.md)。

### Iteration 457：移除未知 API 路径的隐式 Go 转发

Rust API 网关现在只把契约中逐项登记的 61 条 Go 兼容路由转发给 Go；未知路径或缺少 Rust handler 的 Rust 声明路径返回明确 404。HEAD 继续继承同路径的 GET 归属。契约审计器解析固定 allowlist，并将其与 Go AST 路由清单逐项比对；新增 Go 路由必须同时进入人工审查的白名单。eSIM 健康接口在未选中读卡器时仍保留显式 Go 兼容回退。此边界加固没有移除业务 Go 依赖或增加计分。验证详情见[Iteration 457](../../iteration-457-progress.md)。

### Iteration 456：将 REGISTER renewal 信号接到短信事务排空状态

Rust session consumer 现在将 `RegistrationRenewalDue` 与普通 SIP event 区分。它报告当前待确认 delivery-report transaction 数量；最后一笔报告完成时，在 `ReportFinished` outcome 上标记 renewal 等待的报告已排空。此标记不代表整个 actor 已 quiesce，也没有建立受控 IKE/IMS teardown/re-register owner，故生产 gate 与 P3/P4 进度不变。3 个状态测试覆盖空队列、多个报告排空、无 renewal 的普通完成。详见[Iteration 456](../../iteration-456-progress.md)。

### Iteration 455：将授予租期送入 IMS 事件运行时

`RegisteredSipRuntime` 现对注册 response transaction 与 IMS stream 同时等待，并消费成功 REGISTER 得到的一次性 renewal deadline。触发时发 `RegistrationRenewalDue` advisory event，不自行终止当前会话，避免跨过活动呼叫或短信 transaction；真实 native lifecycle consumer 仍缺失，不能将其算作受控恢复或生产接管。Rust userspace TCP 测试验证通知只发一次且后续 SMS transaction 仍可完成。详见[Iteration 455](../../iteration-455-progress.md)。

### Iteration 454：接入 IMS REGISTER 授予租期

Rust coordinator 解析最终实际发送的 REGISTER 请求 Contact/Expires，并用相同请求绑定核验 200 响应 Contact 和全局 Expires。唯一匹配且未撤销时，session 持有授予秒数与续期 deadline；响应缺失、歧义、不同绑定或 expires=0 不生成期限。测试覆盖 Contact 绑定优先、global expiry fallback、错绑与撤销。此 deadline 尚无 production consumer，不会触发 REGISTER refresh 或 session recovery，因此不计 P3/P4 门禁、不增加加权迁移分。详见[Iteration 454](../../iteration-454-progress.md)。

### Iteration 452：执行 carrier IKE 远端证书验证要求

Rust `NativeCarrierTunnelConfig` 现在严格解析并保留 `ims.ike.validate_remote_certificate`。Three UK `234/20` 的 published flag 不再在 native session context 边界丢失；由于 Rust SWu 当前不校验 ePDG 证书链和 IKE AUTH 签名，native startup 对该 flag 返回 `CertificateValidationUnavailable`，且在 heartbeat/socket/network I/O 前退出。测试覆盖 Three UK flag 保留、CTExcel false、unsupported policy fail-closed。`cargo fmt --all --check`、`git diff --check`、`proletarius-vowifi` 182 tests、`proletariusd` 1456 passed / 3 ignored 通过。没有实网/真机验收，P3 和总进度不增加。

`api-contract-inventory.json` 是源码提取结果，`api-contract-review.json`
是动态路由和源码审阅记录。校验工具发现清单漂移、未解析调用或不一致的
Rust 路由归属时返回非零；不会启动后端、App、设备或发送短信。

历史源码快照（Iteration 309，2026-10-10）：334个method/path契约，Rust273、Go61（含49个GET派生HEAD alias）；127个Swift HTTP调用点、327个审阅范围。Iteration 305核对了 Go `moduleSIMMu` 的跨业务SIM/APDU事务边界，新增统一Rust物理卡仲裁器工作包；本轮为审计，不改变迁移计分。Rust拥有短信清空/删除/按发送方删除策略、普通USB模块eSIM Profile改名/删除/切换/下载公开路径，以及模块 VoWiFi 操作结果公开 GET/HEAD 与契约校验。普通USB模块下载由Rust运行LPA安装、进度和确认流程；仍依赖认证Go适配器停止Profile切换前的模块VoWiFi，Go operation observer、非USB兼容回退和部分概览缓存也仍在。VoWiFi启停执行和结果快照仍依赖Go。Iteration 210 将 B1 NBLE 专用认证/分帧接入 Swift CoreBluetooth 系统桥，不改变 Rust 迁移分；用户确认参考项目真机验证通过，Switch 仍待实机复验。Iteration 211 新增的 Rust IMS SIP typed-session boundary 尚未接入生产调用路径。Iteration 297 移除Go→Rust短信易变快照发布和Rust内部接收桥；公开短信列表只组合耐久归档与Rust USB预览，Go仅按Rust选定的稳定ID修剪自身重复缓存，Go接收/归档生产者及实体存储工作流仍未迁移。Iteration 298 将租约内BLE AKA APDU移至Rust BLE桥，退役无消费者的Go AKA lease endpoint；Iteration 299 将 readiness 中已迁移的租约内 BLE AKA 桥正确标为具备，但没有开启生产 native session。Iteration 300 修复验证脚本对旧工作区路径的硬编码；不改变产品逻辑。 Iteration 310 将 Rust 原生 AKA/身份 blocking worker 计入 reader reservation admission；Iteration 311 将 worker admission 按 reader 隔离，不同物理 reader 可并行，同一 reader 与 N待 reservation 互斥。未接入 Gate 的直接 Go APDU 消费者仍需迁移，不增加加权进度。 Iteration 309 将 N待 reader reservation acquire 收紧为必需的 reader/generation 身份绑定，并拒绝错误 JSON 类型与超界值；仍不接管 APDU owner 或跨进程仲裁，不增加迁移分。Rust-only结构退出审计仍阻塞于7类构建/运行依赖，包含256个生产Go源文件。Go仍持有跨进程lease lifecycle；keyed lifecycle actor、protected IMS runtime、live SMS/call runtime 均未就绪。计数是源码盘点，不代表端到端 Go-free。

历史源码清单（Iteration323）：336 method/path，Rust276 / Go60，127 Swift调用点，342审阅范围。完整离线门禁、Go并发检测与扫描器43项测试通过；Rust进一步接管reader选择/generation及不可变转换回执，PC/SC以精确物理session/成功关闭证明衔接。Go仍保留物理适配、authMu、native lease/watchdog与在线session，整体加权31.4%不变；此轮源码不在冻结至322的2.97 DMG内。详见[Iteration323](../../iteration-323-progress.md)。

Iteration 178 历史源码清单曾登记 311 个 method/path 契约：245 个由 Rust 处理（含 40 个公开 GET 派生的 HEAD 别名，以及内部 UDS modem actor 的 13 个 POST 动作、IMS SMS transaction actor 的 10 个动作、7 个 module-radio 动作、7 个 call-mode ADB 动作、8 个 reader-pcsc 动作、2 个 reader-hotplug 动作、2 个通用 profile-notes 存储 RPC、2 个 reader-BLE APDU 动作、3 个公开 BLE 操作路由、1 个 VoWiFi 代理检查路由、2 个认证 N待操作认领/状态回调、N待恢复策略 RPC、重连重试 RPC、IMS 注册续期 RPC及两个对应的到期回调），66 个由 Go 处理；扫描报告 127 个 Swift HTTP 调用点和 287 个审阅范围。此为源码路由与调用点清点，不是迁移完成百分比。扣除 HEAD 派生项，独立 method/path 实现为 Rust 203、Go 66；HEAD 沿用对应 GET 的处理归属，不能算成新业务接管。Rust 路由包含聚合、协议诊断及持久化端点，例如 device-snapshot、native-session-plan 和 N待提交仍涉及 Go 业务；公开设备短信发送虽已由 Rust 编排，仍调用 Go 固定硬件路由桥。内部 modem UDS 契约已按具体方法/路径纳入清单，包含 sms-receive 配置/scan 及 initialize 入口；Rust直接处理 CMTI 调度、typed ingest 和六步串口初始化，初始化能力独立协商，就绪前不读删短信。Go仍提供设备采集、scan请求及其他设备生产者；RF策略、偏好唯一writer、启动屏障、业务预约、VoWiFi射频恢复与物理执行已由Rust接管，Go只保留相关业务适配。新的私有call-archive/observe由Rust聚合三来源事实并归档；Go原状态机仍提供观察及有界队列，不代表SIP/媒体已迁移。USB自动接收的inbox/refresh/prune由Rust处理，实际USB AT句柄和SM/ME事务也已由Rust owner提供；Go原始snapshot桥删除，七项私有USB AT动作之外又新增完整device-status采集，固定session/lease内完成14步或RF-off的8步并返回16字段；Go保留显示名/已存号码、配置和radio/eSIM/AKA/语音业务，新增私有storage-operation完整执行USB短信usage/clear/delete/delete_sender，绑定固定session/lease、卡身份和全部PDU来源；新增archive/delete-ids只清冻结ID集合。通话模式新增Rust私有状态分类、运行时缓存与generation分配、ADB部署准备及D4/UAC route生命周期；Rust还通过严格的 `voice_facts`、`usb_interfaces`、`call_interfaces` 阶段持有IMS/VoLTE、USB ADB/UAC及最终call-interface readiness predicates，Go的来电准备、重启确认和备份还原状态消费Rust结果。Go仍保留公开状态/cache、事实采集、USB transition lease及整体恢复编排。GET /api/health现在由Rust生成兼容形状的公开响应和device-snapshot投影，但经认证的私有Go适配器仍提供USB/eSIM观测事实；显式Go兼容模式继续用旧处理器。 Iteration177将受监督串口typed query与USB状态投影的PLMN显示名映射归入Rust共用模块；Iteration178在既有认证modem actor契约内增加device-info snapshot查询，不新增HTTP/UDS method/path，Go继续写设备缓存；Go QMI/MBIM及独立兼容路径仍保留Go映射。
所有 Rust 条目的迁移状态均标为 `rust_handler_present_end_to_end_not_certified`。Iteration 168新增 `POST /api/internal/multistandby/runtime/recovery-plan`；Iteration 169将逐 Profile 故障计数和稳定在线累计时长持久化到Rust runtime journal；Iteration 170由Rust持有启动恢复5秒轮询及Go代次去重；Iteration 171将观察事实归一化与持久报告事件状态推导迁入Rust；Iteration 172迁入维护节拍和短信耐久队列入队；Iteration 173新增逐Profile耐久重试计时、代次栅栏及确定性错峰；Iteration 174新增独立的IMS REGISTER续期deadline持久化与session-generation callback；Iteration 175迁移公开健康响应投影并让device-snapshot复用它，同时由Rust分配并持久化N待重连尝试代次；Iteration 176将公开 `GET /api/status` 响应投影迁入Rust，并复用至device-snapshot；模块AT串口候选发现和有界探测也迁入Rust modem actor；Iteration177迁移受监督串口operator typed query与USB状态投影中的PLMN显示名映射，并通过冻结Go/Rust状态差分。Go仍提供真实Profile session、USB/eSIM健康事实和硬件生命周期，且显式兼容模式保留旧路径。

**历史加权迁移进度（Iteration 314，2026-10-10）：31.4%**，累计14.920/47.5计划人周等值；P2A为4.110/5.5（74.7%），P2B为5.400/8.0（67.5%），P3为1.640/8.0（20.5%），P4为0.750/4.5（16.7%），eSIM/LPA为1.275/5.5（23.18%）。最近计分项仍为Iteration209；Iteration298把租约内BLE AKA APDU移至Rust并退役无消费者Go endpoint，但Go reader lease lifecycle仍在、native IMS readiness仍关闭，因此不增加加权迁移分。Iteration 210 的 B1 Swift BLE 系统桥和Iteration 211 的未接生产 SIP boundary均不计后端迁移分。最近门禁：Rust workspace 1203 passed / 3 ignored；契约清单336条（Rust275/Go61，含49个HEAD alias）、127个Swift调用点、327个审阅范围。Iteration 301–303为EVS/RTP辅助实现和通话状态边界加固，Iteration 304将AT诊断错误映射移交Rust，Iteration 305审计SIM/APDU跨业务事务锁，Iteration 306核对reader lease消费者，Iteration 307将健康检查中两个已确认错误分支的响应映射迁入Rust；Iteration 312–314逐步扩展reader operation admission至托管卡操作、BLE热插拔维护和USIM信息刷新，但Go `authMu`/APDU及reader lifecycle owner仍在，不计加权进度。Rust-only退出检查仍因默认Go fallback和177条Go owner/dependency路由失败；完整Go-free构建/运行、设备与运营商验收仍未完成。Iteration 314复跑完整迁移门禁通过：Rust workspace 1203 passed / 3 ignored；Go vet/全包测试、carrier、USB、radio、HTTP method/HEAD、codec及IMS MESSAGE差分通过。Go仍持有SIM/APDU互斥锁与执行桥。详情见Iteration 189–314进度记录。进度按计划人周估算（误差约±3个百分点），路由数不作为迁移完成率。

Iteration153新增两个私有Rust action：N待`inbound-event`由Rust完成原始事件归一化、profile-scoped网络重试去重、状态报告关联和multipart冲突回退；`reader-device-identity/resolve`在私有token校验后读取或原子创建兼容`imei.json`。Go仍保留IMS socket/event lifecycle和Go-only身份兼容路径，不能据此宣称通话、短信在线收发或VoWiFi全链路已经脱离Go。

Iteration154在既有 `POST /api/internal/ims/sms-transactions` 私有路由中增加 `prepare_delivery_report` action。Rust现在生成入站短信 RP-ACK delivery-report SIP MESSAGE 帧，并与外发 MESSAGE 共用 session CSeq owner；Go仍先发送入站 SIP 200、保持耐久回调顺序、经当前注册流发送帧并派发/匹配最终 SIP 响应。该 action 不增加 method/path 数量，也不表示IMS event/socket lifecycle已迁Rust。

Iteration156在既有认证路由新增`decode_incoming_delivery` action：Rust接管IMS入站RPDU/TPDU解析和`IncomingSMSEvent`字段组装；Go仍负责持久化回调和RP-ACK顺序。同期BLE scan/connect/disconnect公开HTTP处理器迁入Rust，调用Go私有读卡器租约路由协调共享操作，再访问Swift CoreBluetooth桥。Iteration157新增`decode_and_store_incoming_delivery`：Rust一次完成解码和 N 待耐久入库，Go不再序列化入站事件或再调用 store/inbound-event；callback只有收到`store_state`才允许现有RP-ACK流程继续。普通读卡器IMS保持decode-only。SIP session/立即200/注册stream仍由Go处理；无新增method/path，IMS transaction actor action数继续扩展。

## 离线运行

在 Switch/src 执行（Python 3 和本机已有 Go 编译器；不下载依赖）：

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-rust-migration-contracts.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/tests/test_rust_migration_contracts.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-rust-migration-contracts.py --require-rust-only
```

前两项当前应通过，第三项当前**必须失败**，因为默认 Go fallback 和 Go
业务端点仍存在。这项退出检查仅验证路由层，没有替代无 Go 的干净构建、
设备验证、数据升级与恢复验收。Go AST 小程序只使用标准库解析源码；
`GOPROXY=off`、`GOSUMDB=off`、`GOTOOLCHAIN=local`，编译缓存、临时文件
和自测临时源码均落在 `Switch/build/optimization/all-rust-contract-tool/`。
不生成源码目录里的 `__pycache__`。

## 自动提取与人工范围

- Go：解析 `backend/**/*.go` 的 AST，排除 `_test.go`、third_party、vendor
  和隐藏目录。覆盖函数及包初始化闭包中的 `HandleFunc`/`Handle` 调用，
  不受注释、字符串内的伪代码影响。字面量 method/path 自动登记；变量、
  拼接注册或未支持的 mux pattern 直接报错。未按目标平台过滤 build tags，
  因此这是保守源码范围，不是特定编译配置的可达性证明。
- Rust：提取 main.rs 的 method/path 元组分发、ownership.rs 的显式
  `matches!` 声明；普通与 `r"..."`、`r#"..."#` 等原始字符串均支持。
  注释及 `#[cfg(test)] mod ...` 排除。新增静态 handler 无 ownership、
  ownership 无记录 handler 均报错。所有生产 `.rs` 文件还登记 `/api/`
  字面量引用，发现新增引用和路径常量变化。
- 动态 Rust：人工逐条确认短信导出 token、N 待消息删除、内部 store
  action 和 reader-reservation 前缀，记录具体方法、模板、来源函数与
  限制。不能用“整个前缀都是 Rust”替代具体可用端点。相关 dispatch、
  ownership 和 action whitelist 函数做去注释、去空白后的 SHA-256
  指纹；变化后校验失败并显示需审阅的新指纹。
- Swift：枚举 `get/getData/send/upload` 无标签首参候选，识别固定路径、
  method 参数、查询串与简单插值模板。N 待 helper、reader eSIM 前缀和
  变量路径显式登记展开结果；动态构造者、APIClient 方法默认值与两个
  非 HTTP 同名调用所在文件有完整源码指纹。未解析调用报错；每个解析
  出的调用必须能匹配已有 method/path。诊断比较字符串保留为引用，不
  当作实际调用。Swift 全量 API 字面量也参与漂移比较。

Rust/Swift 提取器是针对当前路由形式的受限词法工具，不是完整编译器或
类型检查器。引入新客户端方法、新路由宏、不同分发框架、外部 test module、
跨文件动态构造时，应扩展提取器和负例测试，不能只更新 JSON。
Go 注册器别名、任意运行时构造和所有业务依赖的传递闭包均未自动证明。
Go API 引用保留了源码证据，但没有冒充已验证的调用边。

条目含来源、HTTP owner、方法/路径、类别、App/内部受众、Swift 消费者、
迁移状态及已确认的 Go 依赖。`go_dependencies` 是确认的实例，不是依赖
全图；空数组不表示已达到端到端原生接管。没有直接 Swift 消费者的端点
明确标记需要核对内部后端、测试或兼容客户端，不能据此删除。

## 更新流程

1. 先运行带 `--diff` 的检查，查看增加、删除、改名和消费者变化。
2. 若出现 `MANUAL REVIEW REQUIRED`，核对该函数/文件的真实分发和路径
   构造，同时更新动态条目或依赖说明；确认后再手动更新 review.json
   对应 SHA-256。工具不会替人自动认可这些变化。
3. 新写法先补反例测试；移除路由要确认所有消费者、旧数据和兼容需求。
4. 执行 `--write-inventory` 显式生成清单，然后再跑普通一致性和工具自测。
   `--write-inventory` 仍拒绝指纹不一致、未知调用和 handler/owner 漏项。

指纹忽略注释/空白，清单仍记录定位行号；仅行号漂移也需要显式刷新清单。
新增业务逻辑可能触发文件范围复核，这是动态 Swift 构造的保守边界。
所有路由 JSON 响应、错误码、请求字段及实际 Go-free 业务行为需要后续
契约 fixture 和阶段验收；本轮没有将它们视为已覆盖。Go GET 隐式接受
HEAD、405/未知路径行为等也必须在最终切换的兼容性验收中核对。

## 工具反例

自测覆盖新增 Go 路由、Go 动态注册、包初始化注册、Go raw 字符串、
Rust 缺失 handler/ownership、新 raw 字符串路由、注释/测试排除、
字符字面量花括号、Swift 缺路由/未知动态路径、动态构造指纹变化、
手工映射缺指纹、插值与显式 DELETE，以及 Go fallback/依赖退出门禁。
通过仅表示这些已测的漏项会被发现，不宣称静态扫描可以证明任意程序正确。

## Iteration 089 审阅范围补充

本轮补充模块接收队列、typed ingest、会话栅栏、实际关闭等待和Go UDS适配器的人工审阅。Go `manager.go` 中的反引号raw string超出现有通用scope词法器能力，因此未伪造该文件指纹；其Start/configure/URC归属分流、健康状态和退出接线由人工检查及Go全包race覆盖，不能声称自动scope门禁已经覆盖该文件所有变化。Go AST路由提取与此scope词法限制是两项不同能力。详情见[Iteration 089](../../iteration-089-progress.md)。

## Iteration 090 审阅范围补充

加入initialize私有契约，人工复核8项已有scope并新增9项，合计103项。新增范围包含初始化状态机、Go严格响应验证、open/initialize/close/退出接线；actor全文件指纹用于发现enum/state/独立能力字段的变化，不替代语义或实机验收。manager中的六步调用替换、实际协程退出等待及watchdog仅恢复一次仍按上述限制执行人工审查及race回归。一致性与16项工具自测通过；Rust-only检查仍拒绝默认Go fallback及124条Go归属/依赖路由。详情见[Iteration 090](../../iteration-090-progress.md)。


## Iteration 091 审阅范围补充

新增私有`POST /api/internal/call-archive/observe`，原始事件仍依赖Go设备/IMS生产者；旧ingest保留为兼容入口。新增15个可解析整文件scope，总计118项，覆盖Rust事实聚合与归档、Go有限桥/服务接线及Swift可选错误展示；本轮既有dispatch、生命周期和接线scope同步人工复核。

Go `manager.go` 与 `imscore/call_runtime.go` 的raw string超出现有通用scope词法器能力，两者由人工逐项审阅及全包race覆盖，不伪造自动指纹。Go AST路由提取仍正常；该限制不代表所有观察者行为已由静态工具证明。最终清单一致性及16项工具自测通过；Rust-only按预期拒绝默认Go fallback及125条Go归属/依赖路由，没有scope或清单漂移。完整验证结果见[Iteration 091](../../iteration-091-progress.md)。


## Iteration 092 审阅范围补充

新增Go `POST /api/internal/usb-sms/snapshot`及Rust私有`GET /api/internal/usb-sms/inbox`、`POST /api/internal/usb-sms/refresh`、`POST /api/internal/usb-sms/prune`。原始采集仍依赖Go固定句柄/设备锁，不能以Rust调度入口代替物理owner的退出证明。新增13项可解析scope，共131项；人工复核USB原始有界I/O、producer/session边界、Rust单worker与Inbox分区、按ID清理及清理版本校验、Go公共适配及删除整组SIM锁、各层手动刷新期限和Swift精确白名单。

清单写入/一致性及16项工具自测通过；Rust-only按预期拒绝默认Go fallback及129条Go所有权/依赖路由。最终验证结果与静态扫描范围限制见[Iteration 092](../../iteration-092-progress.md)。Go manager及imscore/call_runtime的既有raw string词法限制仍按人工审查/race登记，不伪造自动指纹；本轮没有以静态清单宣称真实设备验收通过。


## Iteration 093 审阅范围补充

Go原始USB短信snapshot生产接口移除，新增Rust私有USB AT七项POST动作：open/status/acquire/release/close/command/configure。Rust现持有实际AT句柄及本地SM/ME采集；Go仍提供设备/radio/eSIM/AKA/语音业务和配置，七项入口均按现存上游依赖保守登记，不视为端到端Go-free。删除3项随旧Go源码退出的scope，新增9项owner/transport/RPC/事务/语音门禁/ADB适配范围，共137项；全部变化scope人工复核。

清单为210条method/path（Rust110、Go100）和127个Swift调用点；write/check及16项工具自测通过。Rust-only按预期拒绝默认Go fallback和135条Go归属/依赖路由，未降低退出条件。完整验证和限制见[Iteration 093](../../iteration-093-progress.md)。


## Iteration 094 审阅范围补充

新增Rust私有`POST /api/internal/usb-at/device-status`，固定session/SIM lease内完成整个USB设备状态采集和投影。调用仍依赖Go生命周期/偏好/显示名与缓存，按Go依赖保守登记，不将入口改为Rust等同端到端Go-free。人工复核原有5项变化scope，并新增Rust collector、Go状态适配器两个scope，总计139项。

当前211条method/path（Rust111、Go100）和127个Swift调用点。严格请求字段、I/O期限、实际worker持有、VoWiFi inhibit、旧session隔离、取消准入与持lease发布缓存均已交叉审阅；85组Go独立投影→Rust真实collector差分加入永久门禁。最后清单一致性、工具自测及Rust-only负向门禁结果见[Iteration 094](../../iteration-094-progress.md)，不以静态清单代替硬件验收。


## Iteration 095 审阅范围补充

新增私有 `POST /api/internal/usb-sms/storage-operation` 与 `POST /api/internal/sms/archive/delete-ids`。前者由 Rust 执行完整物理存储计划和来源证明协调，仍依赖 Go 公开适配、模式准入与固定 lease；后者仅执行冻结 ID 集合的单次耐久历史清理。单条/发送方/清空不再通过旧 Go AT 多命令计划，不能据此宣称 Go 公开业务已退出。

新增 8 项完整文件 scope，复核 11 项已有变化范围，当前共 147 项。复核覆盖固定卡/session/PDU证明、部分确认和未知结果、选择与恢复、生命周期锁与实际关闭等待、隐藏发送方、旧结果不清新会话、精确本地历史同步及各层方法/路径期限。清单共 213 条 method/path（Rust 113、Go 100）、127 个 Swift HTTP 调用点；写入/一致性检查与 16 项工具自测通过。Rust-only 按预期拒绝默认 Go fallback 及 137 条 Go 归属/依赖路由，详细日志与限制见 [Iteration 095](../../iteration-095-progress.md)。


## Iteration 096 审阅范围补充

公开 GET/POST `/api/module/flight-mode` 从 Go 转交 Rust，并新增 7 个私有 POST `/api/internal/module-radio/` 动作：configure/query/bootstrap/refresh/reservation/vowifi/action。Rust持有偏好、策略、预约/intent和物理计划，Go调用者仍在生产；新增条目按实际业务依赖登记，不能据此宣称Go已退出。

删除旧 Go policy 文件对应的 1 项 scope，新增 16 项，当前共 162 项；复核覆盖实际USB/serial会话、原子抑制门禁、producer撤销、未知结果和token/sequence栅栏、同会话启动恢复、通话协调以及Swift精确请求期限。当前清单为 220 条 method/path（Rust 122、Go 98）、127 个 Swift HTTP 调用点。最终写入/一致性检查及 16 项工具自测通过；Rust-only 按预期拒绝默认 Go fallback 及 144 条 Go 归属/依赖路由。flight-mode 本轮仅验证 GET/POST；旧 GET 隐式 HEAD 在新入口仍落 Go fallback，属于待补的 P0 方法兼容边界，不能宣称全部旧 HTTP 方法等价。完整实现和验收证据见 [Iteration 096](../../iteration-096-progress.md)。


## Iteration 097 审阅范围补充

公开 `public_http.rs` 的57条路径方法表由独立抽取的Rust handler及同path Go注册共同校验：漏项、多声明、重复、非法方法/模板、HEAD无GET、内部路径和缺整文件指纹均拒绝。20个HEAD条目明确标为GET派生alias并继承owner及Go依赖；它们不是新handler或额外迁移分。旧Go派生21条路径保留405/Allow；纯native错误和Go专属方法仍沿原路径。

外部测试源码现在依据真实 `#[cfg(test)] mod` / literal `include!` 引用图识别，不按文件名猜测；共享生产引用仍保留，复杂未知条件/路径拒绝审阅。当前排除15项只被测试引用的源，包括两个新增HTTP测试模块。复核5项已有变化scope并新增`handle_request_inner`及`public_http.rs`整体范围，合计164项；两项已有测试声明的hash变化经旧提取规则复算，确认未改变其生产代码。

当前清单240项（Rust142、Go98），含20项HEAD alias，Swift127调用点。实现、209组差分和验收限制见[Iteration097](../../iteration-097-progress.md)；URL清理/转义/尾斜线仍未验收。

Iteration097 最终 write/check 及 41 项工具自测通过；Rust-only 仅因默认 Go fallback 和 151 条 Go 归属/依赖契约而按预期失败，较上轮增加的 7 项来自 HEAD 继承原 GET 依赖。

## Iteration 098 审阅范围补充

新增 8 个私有 `POST /api/internal/reader-pcsc/` 动作：open、status、acquire、renew、release、transmit、reset、close。它们有明确的方法、动作白名单、严格请求字段和 160 KiB 请求上限；停止期间仅接受 status/release/close 清理。Rust 持有真实 PC/SC 句柄、完整系统事务、APDU 执行、租约到期和复位/关闭确认，Go 与原生 LPA、身份读取、AKA 共用固定 capability。下载预检和安装、通知 AID 遍历、身份核对和 AUTH 分别保持在完整事务内。

Go 仍提供读卡器选择、业务 generation、准入、缓存、切卡收尾及 BLE 兼容。原生租约回调使用限定生命周期的 Context capability，保留 authMu，不再通过全局借用开关放行其它 goroutine；复位仍遵循 ESTK 专属规则。未知结果不会触发 APDU 重放，关闭未确认会保留实体资源责任；旧 producer、旧 session 和延迟关闭不能覆盖新会话。公开读卡器不存在错误保持 `409 / reader_unavailable`，私有 owner 的错误码和状态仍独立。

新增 21 项 owner、系统桥和 Go/Rust 消费者 scope，复核 20 项已有变化范围，当前共 185 项；原生 owner 使用完整生产文件指纹，包含 session 发布、close tombstone 与 producer 的同锁核验。清单共 248 条 method/path（Rust 150、Go 98），其中 20 条为 HEAD alias；Swift HTTP 调用点仍为 127 个。8 个新私有动作全部保留 Go 业务调用者依赖，不能据此宣称端到端 Go-free。

最终 write/check 均通过，41 项工具自测通过；Rust-only 仅因默认 Go fallback 和 159 条 Go 归属/依赖契约而按预期失败，没有指纹或清单漂移。新增 2 个显式测试模块后，共排除 17 个仅测试 Rust 源文件。离线 fake-I/O、错误分类和取消/关闭竞态验证不等同于实体卡复位、eSIM 写卡、IMS 长时在线或最终 Go 退出验收。契约工具运行结果见 `Switch/build/optimization/2026-10-06-pcsc-contract-*.log`；源码清单一致性与完整应用构建各自提供独立证据。

## 2026-10-07 审阅范围补充

新增 Go 私有 `POST /api/internal/reader/pcsc-presence-lost`：Rust 对空闲且没有租约的 PC/SC 会话，在两次成功扫描均确认缺失后，按原 producer、reader、session 与 sequence 栅栏关闭实体句柄。实体关闭确认后才向 Go 发送同一身份的业务清理请求；Go 需要确认旧 VoWiFi 与 N 待 runtime/owner 已退出才回执。超时、扫描失败、实体关闭失败或业务清理失败不会放行新会话。Go 仍负责读卡器选择、业务代次、VoWiFi/N 待收尾和 BLE 监测；离线测试不能证明真实拔卡时系统调用一定完成。新 Go 路由是未迁移依赖，不增加 Rust 接管数。

设备短信转发生产者改为读取 Rust 的 `/api/sms` 同源持久归档与 Go/USB 临时收件分区，不再从 Go 的公开 `/api/sms` 拉取。Go 临时快照携带子进程 producer 身份；旧子进程退出后 Rust 原子轮换身份。转发入队与账本落盘在同一身份锁下复核，防止旧代请求跨异步等待在退出后提交。短信硬件采集仍有 Go 依赖，此处只收紧数据源与提交边界。

同时审阅了公开路径规范化及通话/短信客户端模型变化。Go ServeMux 离线抽样核对了重复斜杠、`.`/`..`、查询串、HEAD、CONNECT 和转义 `%2e` 的 307/Location/响应体行为；这不是任意 URL 的完整等价证明。新增 PC/SC presence、Go handoff、BLE 监测、N 待收尾、短信转发 producer 与提交 guard 等整文件指纹，当前共有 **191** 项审阅范围。

最终 `--write-inventory`、普通一致性检查及 **41** 项工具自测均通过。清单为 **249** 条 method/path（Rust **150**、Go **99**，含 **20** 个 HEAD 别名）与 **127** 个 Swift HTTP 调用点。`--require-rust-only` 按预期只因默认 Go fallback 和 **160** 条 Go 归属/依赖路由失败，没有额外的源码指纹、路由或调用点漂移。日志位于 `Switch/build/optimization/p0-contract-{write,check,selftest,rust-only}-2026-10-07.log`。该门禁仅覆盖路由层；完整无 Go 构建、系统桥行为、实卡热拔插、长时 IMS/VoWiFi 与数据升级仍须分别验收。

## Iteration 102 审阅范围补充

Rust 新增公开 `GET/PUT /api/esim/notes` 与私有 `POST /api/internal/profile-notes/phone`、`POST /api/internal/profile-notes/remember-phone`。Rust 统一持有旧 `profile-notes.json` 数据格式、读取/原子写入和号码回填；Go 删除公开读写及本地文件 writer，只通过认证 UDS RPC 暂时调用号码 lookup/remember。两条公开路由从 Go 转由 Rust，另有公开 GET 的 HEAD 派生项及两个 Rust 私有兼容调用。持久化保留既有字段格式，限制文件/请求大小、原子替换、权限、符号链接与损坏文件处理；该范围不等同于 eSIM LPA 或其他 eSIM 业务已迁移。

最终清单为 **252** 条 method/path（Rust **155**、Go **97**，含 **21** 个 HEAD 别名）、**194** 个审阅范围及 **127** 个 Swift HTTP 调用点；普通一致性检查与 **41** 项检查器自测通过。`--require-rust-only` 按预期因默认 Go fallback 和 **158** 条仍有 Go 所有权/依赖的路由失败。完整离线回归、Go 兼容调用边界与 Release App 构建见 [Iteration 102](../../iteration-102-progress.md)。这两个持久化 API 的所有权切换不把整个 eSIM 工作包计为完成；仍按粗粒度 24.9%（±3 个百分点）报告，不从路由数量推导进度。

## Iteration 106 审阅范围补充

`GET /api/network/services` 由 Rust 调用系统 `networksetup` 并负责解析、旧 JSON 投影与错误响应；Go 原 GET handler 已移除。为保持 `module` 标记，Rust 仍读取 Go `GET /api/health` 中的 USB 产品名；网络顺序变更 `PUT /api/network/services-order` 继续由 Go 所有。HEAD 是 Rust GET 的派生别名，继承上述 Go 依赖边界。

刷新来源指纹后，契约清单为 **256** 条（Rust **166**、Go **90**，含 **25** 个 HEAD 别名）、**199** 个审阅范围；扫描报告 **127** 个 Swift HTTP 调用点。普通检查和 **41** 项工具自测通过。Rust-only 门禁仍因默认 Go fallback 及 Go owner/dependency 路由而失败。route census 只衡量入口归属，不能替代 Go-free 构建、系统桥行为或硬件验收。呼入按钮布局修复已在 [Iteration 106](../../iteration-106-progress.md) 记录，不计为 Rust 迁移进度。


## Iteration 107 审阅范围补充

`GET /api/network/traffic` 的只读 `networksetup`、`ifconfig` 与 `netstat` 调用、模块网卡选择、计数解析、旧响应投影和会话基线现由 Rust 执行；Go 原采样 handler 与基线已删除。Rust 仍从 Go `/api/health` 读取 USB 产品名，网络服务顺序 PUT 仍由 Go 执行；派生 HEAD 继承同一依赖。命令有时限和输出上限，不改变网络配置、不操作 SIM。

刷新审计指纹后，契约清单为 **257** 条（Rust **168**、Go **89**，含 **26** 个 HEAD 别名）、**200** 个审阅范围；扫描报告 **127** 个 Swift HTTP 调用点。Rust daemon **704 项通过、3 项忽略**，检查器 **41** 项自测通过，普通契约扫描通过。整体计划完成估计仍显示 **25.1%**（11.945/47.5人周，四舍五入后百分比未变）；本地没有进行网络/实体设备测试，也没有生成 DMG。

## Iteration 108 审阅范围补充

`POST /api/network/check-4g` 现由 Rust 在本机认证分发中直接处理，使用有界 `/sbin/route` 调用和共享的 ifconfig 解析；Go 原 handler 已移除。它没有 Go health 数据依赖，但更广泛的 AT 网络诊断和服务顺序写入仍由 Go 执行。原有根据活动非 `en0` 以太网接口推断 4G 的逻辑被保留，没有声称已识别具体模块服务。

契约清单为 **257** 条（Rust **169**、Go **88**，含 **26** 个 HEAD 别名）、**201** 个审阅范围和 **127** 个 Swift HTTP 调用点；普通扫描和41项工具自测通过。整体进度为 **25.2%**（11.970/47.5人周）。Release App **2.86/build 286** arm64 构建与严格签名验证通过；无 DMG。


## Iteration 109 审阅范围补充

`PUT /api/network/services-order` 的 Rust handler复用受限 networksetup清单，验证至少两个名称、空值/未知服务和重复项，再经POSIX shell安全转义与Base64构造管理员授权的 `osascript` 请求。Go原handler、请求校验和命令构造均已移除。方法策略维持该路径仅PUT，其他方法仍回报405/Allow；请求体采用1 MiB上限、总执行期限3分钟，命令标准输出和错误输出分别限1 MiB。

完整 `proletariusd` 测试 **712通过、3忽略**；Go主包测试、`cargo fmt --check`、`git diff --check` 与41项契约扫描自测通过。刷新后的契约清单为 **257** 条（Rust **170** / Go **87**，含 **26** 个HEAD别名）、**201** 个审阅范围和127个Swift HTTP调用点。正常契约扫描通过；`--require-rust-only`仍因Go fallback和其他Go归属/依赖而不能通过。测试没有运行 `osascript`，不会改动实际网络服务顺序。总计划进度更新为 **12.010/47.5≈25.3%**，P2A为约**63.64%**。

## Iteration 110 审阅范围补充

Rust 接管遗留 GET /api/voice/enabled 与已停用 POST /api/voice/enable。前者调用 Rust modem actor 的固定 USBCFG 查询，并兼容读取/更新 voice-enabled.json；查询失败时返回文件中的旧状态。后者保留限长 JSON 校验和原 409 停用提示，不改变模块配置。Go handler、启动缓存与文件writer已删除。

Rust daemon **719通过、3忽略**，Go主包测试、Rustfmt、41项契约工具自测和普通扫描通过。清单为 **258** 条（Rust **173** / Go **85**，含 **27** 个HEAD别名）、**202** 个审阅范围、127个Swift HTTP调用点；UAC只通过离线伪终端验证，尚无实体模块测试。P2A增加0.025人周至64.09%，总进度为 **12.035/47.5≈25.3%**；不生成DMG。


## Iteration 112 审阅范围补充

`GET /api/call-mode/backups` 由 Rust 直接认证处理，读取既有 module-backups 目录并投影 schema 0/2/3；过滤非法标识、符号链接、非普通/空文件及超过64KiB的备份，格式损坏的文件保留为不可恢复条目。Go 删除公开列表 handler；备份创建、导入、删除、导出及硬件恢复仍由 Go 持有，不能把该路由描述为整个备份功能 Rust 化。

Rust daemon 725通过、3忽略；Go主包测试、Rustfmt、41项合同工具测试和正常扫描通过。清单为259条（Rust175 / Go84，含28个HEAD别名）、203个审阅范围和127个Swift HTTP调用点。损坏JSON的报错文本未证明与Go逐字一致；没有访问读卡器或实体卡。Release `Switch.app` 2.86/build 286 arm64构建与严格签名验证通过。P2A增加0.025人周至约64.55%，总体为12.060/47.5≈25.4%；不生成DMG。


## Iteration 113 审阅范围补充

`GET /api/call-mode/backups/export` 的原始文件下载现在由 Rust 直接认证处理。ID 仍受既有文件名模式限制；读取使用 `O_NOFOLLOW`、普通文件检查和 64 KiB 上限，返回原始 JSON 字节及附件/Nosniff/Last-Modified headers。Rust保持条件GET、单段和多段字节Range/416语义；HEAD沿用公开GET别名。Go 的导出API handler与对应Go HTTP测试删除；备份写入、导入、删除和硬件还原仍属于Go。

Rust daemon 728通过、3忽略；备份专项9项通过；Go主包测试、Rustfmt、41项合同工具自测和普通扫描通过。清单为260条（Rust177 / Go83，含29个HEAD别名）、203个审阅范围和127个Swift HTTP调用点。Release `Switch.app` 2.86/build 286 arm64构建成功并通过严格签名验证；没有访问读卡器/实体卡或生成DMG。P2A增加0.025人周至3.575/5.5（65.00%），总体为12.085/47.5≈25.4%。


## Iteration 114 审阅范围补充

公开 `POST /api/call-mode/backups/import` 从 Go 转由 Rust 负责。该操作先限长并校验备份schema、USB配置和还原命令，再将上传的原始JSON以私有权限写入唯一命名文件，并在返回摘要前重读校验。`null` schema维持Go解码为零值的兼容语义；畸形JSON的解析诊断文案未做逐字兼容承诺。Go仍创建备份、删除备份并执行硬件恢复。该endpoint属于原有Go method/path的所有权转移，契约总数不因迁移增加。

新增Rust测试覆盖原字节保存、权限、唯一名称碰撞、旧schema、校验失败、空body与超限body；Go重复导入实现/测试删除。合同清单保持 **260** 条，owner更新为Rust **178**、Go **82**，HEAD别名29、Swift调用点127。daemon **731通过、3忽略**，Go主包测试与通话接听/拒接回归通过；完整应用2.86/build286 arm64构建及严格签名验证通过。P2A增加0.025人周至**3.600/5.5（65.45%）**，总体进度为**12.110/47.5≈25.5%**。


## Iteration 115 审阅范围补充

确认删除备份接口 `POST /api/call-mode/backups/delete` 现由 Rust 认证处理，Go route 与 handler 已删除。Rust 保留确认字段、1 MiB 请求上限、ID trim/白名单、先读文件再检查占用的顺序、400/404/409/500 状态及成功 JSON。文件校验沿用同一 no-follow reader，拒绝符号链接、非普通文件、空文件及超过64KiB的备份。

删除期间，Rust module-radio service 设置一个同锁互斥标志，禁止 Go 再取得 `call_mode` reservation。为覆盖 Rust daemon 在 Go 硬件操作中途重启的情况，Go 操作在取得 Rust reservation 后还会持有 data root 下的 mode-0600 `call-mode-operation.lock`；Rust 删除必须非阻塞取得同一系统 flock，冲突返回既有409并保留文件。该协调文件不含业务数据。Go 仍创建备份并执行硬件恢复；本项只接管文件删除，不代表通话模式整体迁移。

Rust daemon **734通过、3忽略**，删除接口/互斥专项、Go主包 race、`go vet`及共享锁文件独占/释放/符号链接负例通过。迁移工具41项自测、普通契约检查、Rustfmt与`git diff --check`通过。契约清单为260条（Rust179 / Go81，含29个HEAD别名）、203个审阅范围、127个Swift HTTP调用点。Rust-only门禁按预期失败：默认Go fallback仍在，148条路由仍有Go owner/dependency。Release `scripts/build.sh` 构建2.86/build286 arm64 `dist/Switch.app`，严格签名校验通过。没有连接设备、操作实体卡或生成DMG。P2A计入+0.025计划人周至**3.625/5.5（65.91%）**；总进度为**12.135/47.5≈25.5%（±3个百分点）**。完整 Go-free build、模块硬件业务、IMS/VoWiFi与实机验收仍未完成。


## Iteration 116：安全诊断事件与呼入按钮布局

Rust 维护诊断事件的闭合字段模型、序号与256条环形历史。公开 `GET /api/diagnostics/events` 由 Rust 认证处理，兼容 HEAD 与旧方法错误；私有 `POST /api/internal/diagnostics/events/append` 仅允许内部 UDS token 调用，并拒绝未列入 DTO 的字段、原始错误正文及超限请求。Go 事件生产者只向512项有界异步队列提交脱敏枚举数据，后台单worker写入 Rust；队列满时丢弃最旧待处理事件，不影响业务路径。

同时，通话页呼入操作从自适应网格调整为整组居中的水平按钮行，解决接听和挂断按钮相对卡片偏左。Rust daemon **739通过、3忽略**；Go race、`go vet`、15项跨语言事件集成、41项合同工具测试、普通契约扫描、Rustfmt、差异检查及 `Switch.app` 2.86/build286 arm64 签名验证通过。最新清单262条（Rust182 / Go80，含30个HEAD别名）、208个审阅范围、127个Swift调用点。Rust-only门禁仍有147条路由依赖Go所有权或实现；没有实机/实体卡验收，也没有生成DMG。


## Iteration 117：优化契约扫描器词法复杂度

Rust 词法扫描器原先在每个 token 处对剩余源码切片并执行正则，完整 daemon 源码扫描出现持续满核耗时。现使用预编译 regex 在原始字符串偏移处匹配，避免构造反复扩大的后缀副本；20,000行 raw-string 夹具验证字符串/comment边界保持不变。合同工具测试42项通过；完整清单检查4.88秒通过，262条method/path（Rust182 / Go80，含30个HEAD别名）、208个审阅范围和127个Swift调用点均与登记一致。Rust-only门禁仍未完成。


## Iteration 119 审阅范围补充

私有 `POST /api/internal/call-mode/backups/restore-data` 由 Rust 负责按 allowlisted ID 无符号链接读取、限长与 schema/还原命令校验，并仅返回已确认可还原的 DTO。Go 的 API 与后台恢复 worker 不再直接打开或解析备份文件；硬件状态机、当前模块身份/USB匹配和 AT 写入回读仍由 Go 执行。endpoint 需要内部 token，daemon 停止期间拒绝还原读取。

Rust daemon **742通过、3忽略**；Go call-mode backup/operation 定向 race、契约工具42项自测、普通全量扫描、Rustfmt、`git diff --check` 通过。清单264条（Rust184 / Go80，含30个HEAD别名）、210个审阅范围、127个Swift HTTP调用点。Release `Switch.app` 2.86/build286 arm64构建与严格签名通过；没有实机/实体卡操作或生成DMG。

P2A计入 **+0.025计划人周**，由3.685提高到**3.710/5.5（67.45%）**；整体进度由12.220提高到**12.245/47.5≈25.8%（±3个百分点）**。Go硬件状态机、IMS/VoWiFi与最终零Go验收仍未完成。详情见[Iteration 119](../../iteration-119-progress.md)。


## Iteration 120：删除 Go 备份存储旁路

删除 Go 中未注册的 call-mode 备份列表、文件读取、解析和摘要投影实现，消除 Rust 已接管后的第二套文件 owner。Go 保留硬件状态机、安全 DTO 检查与 Rust 私有 UDS 客户端；公开路径和数据格式未变。Go 全包 race/`go vet`、Rust 备份专项17项、合同工具42项、全量清单、Rustfmt、差异检查及 Release `Switch.app` 签名验证通过。清单仍为264条契约（Rust184、Go80，含30个HEAD别名）、210个审阅范围、127个Swift调用点。没有实体卡操作、真实通话或DMG。

此项计入P6 Go退出准备 **+0.025计划人周**，总进度为**12.270/47.5≈25.8%（±3个百分点）**；P2A保持**67.45%**。Go模块硬件状态机、IMS/VoWiFi及零Go终态尚未完成。详情见[Iteration 120](../../iteration-120-progress.md)。


## Iteration 121：备份格式校验收归 Rust

Rust 备份 owner 已校验创建与恢复输入的 schema、时间戳、模块 IMEI、USB 功能位、还原命令和 IMS/VoLTE 字段。本轮移除 Go 对同一备份 DTO 的重复验证及重复测试；备份创建继续经 Rust 私有 UDS 失败关闭，恢复只接受 Rust 返回的已验证 DTO。Go 保留紧邻硬件写入前的当前 IMEI 与 USB VID/PID 匹配，避免有效备份应用到另一台模块。

Rust 备份专项 **18项通过**，daemon 全量 **743通过、3忽略**；Go call-mode 定向 race、Go 主包全量 race 与 `go vet` 通过。迁移契约工具 **42项通过**，扫描保持 **264** 条（Rust **184** / Go **80**，含30个HEAD别名）、**210** 个审阅范围、**127** 个Swift调用点。Release `scripts/build.sh` 构建 2.86/build 286 arm64 `dist/Switch.app`，严格签名校验通过。没有操作读卡器或实体卡，没有生成DMG。

该备份服务校验边界计入P2A **+0.025计划人周**，至 **3.735/5.5（67.91%）**；整体为 **12.295/47.5≈25.9%（±3个百分点）**。Call-mode硬件状态机、其他Go业务、全Go-free构建/运行及设备级验收仍未完成。详情见[Iteration 121](../../iteration-121-progress.md)。


## Iteration 122：IMS/VoLTE 配置写事务迁入 Rust

`call_mode_voice` 现在由 Rust module-radio 在 CallMode lease 下执行：CLCC安全门禁、IMS/VoLTE读取、必要写入与重启生效前回读均为固定计划；终端拒绝停止，写入结果不确定时不重放。Go只调用typed action，不再写这两项QCFG；状态机、USB/ADB/helper及重启恢复仍留在Go。

同轮daemon **750通过、3忽略**；Go主包race与`go vet`、合同工具42项自测及普通契约扫描通过。此项计P2B **+0.025人周**，到3.850/8.0；总计12.320/47.5≈25.9%。详情见[Iteration 122](../../iteration-122-progress.md)。

## Iteration 123：IMS/VoLTE只读状态查询迁入 Rust

移除Go对IMS及VoLTE QCFG的直接读取/解析；Rust新增受reservation约束的只读typed action，调用者可复用CallMode/Call lease，否则申请并释放短CallMode lease。Rust完整daemon **752通过、3忽略**；Go主包race与`go vet`、Rustfmt、差异检查、迁移工具42项测试、264条契约扫描（Rust184/Go80，210审阅范围、127 Swift调用点）通过。Release `Switch.app` 2.86/build286 arm64 deep strict签名有效。产物仍含Go `switchd` 与Rust `switchd-rs`，全Rust目标仍未完成；无硬件测试、无DMG。

此项计P2B **+0.025人周**至3.875/8.0（48.44%）；整体为**12.345/47.5≈26.0%（±3个百分点）**。详见[Iteration 123](../../iteration-123-progress.md)。

## Iteration 124：call-mode USBCFG 查询迁入 Rust

Rust module-radio新增只读 `call_mode_usb_composition_read` 固定计划，在CallMode/Call lease内串行查询并校验唯一USBCFG响应、VID/PID和7个布尔位。Go通话模式状态、启用/还原预检、写后/重启后核验及接口就绪检查改用Rust typed action；Go原始USBCFG查询和响应解析器已移除，写入、ADB/helper和重启状态机仍在Go。

全量离线验证755通过/3忽略，Go race与vet、42项契约自测、普通契约扫描及Release应用构建/签名通过。P2B **+0.025人周**至3.900/8.0（48.75%）；总体**12.370/47.5≈26.0%（±3个百分点）**。无实机操作或DMG。详见[Iteration 124](../../iteration-124-progress.md)。


## Iteration 125：通话模式 USBCFG 写入迁入 Rust

通话准备与备份还原均改用 Rust compare-and-write action；Rust先检查通话状态及预期快照，写入一次后执行 typed readback，异常结果不会自动重放。Go保留目标生成、身份绑定、ADB/helper和重启状态机。

Rust workspace 759通过/3忽略，Go race与vet、42项契约自测、普通扫描和格式检查通过；264条契约归属未变（Rust184/Go80）。无实机写入或DMG。P2B **+0.025人周**至3.925/8.0（49.06%）；整体**12.395/47.5≈26.1%（±3个百分点）**。详见[Iteration 125](../../iteration-125-progress.md)。

## Iteration 134：模块语音运行时就绪缓存迁入 Rust

Go 的 `moduleVoicePrepared` 布尔缓存已移除。Rust 就绪状态绑定 producer、runtime generation 和模块 boot ID；daemon或模块重启、producer替换和旧代次请求均不会复用旧状态。部署成功需要 Rust 完成准备并提交 `prepared=true`，Go 客户端严格验证该字段。全量离线验证 Rust workspace 814通过/3忽略、Go race及vet通过、迁移契约42项自测和普通扫描通过。当前 census 为274项（Rust193/Go81，30个HEAD别名）、127个Swift调用点、221个审阅范围；Rust-only门禁仍因fallback及150条Go所有权/依赖路由失败。Release构建与限制见[Iteration 134](../../iteration-134-progress.md)。无实体设备操作或DMG。

P2B **+0.025人周**至**4.450/8.0（55.63%）**；整体为**12.920/47.5≈27.2%（±3个百分点）**。Go通话模式状态机和设备验收仍未完成。

## Iteration 135：通话模式只读状态分类迁入 Rust

Rust 新增私有认证接口，根据已有 USB/IMS/VoLTE/runtime 事实完成 readiness 分类和下一只读 ADB 探测选择；Go 保留状态缓存、观察、流程编排、设备动作与恢复。离线验证 Rust workspace 818通过/3忽略，Go race/vet、42项契约自测与清单扫描通过。最新清单275条（Rust194/Go81，30个HEAD别名）、127个Swift调用点、222个审阅范围。Release构建 `Switch.app` 2.87/build287 arm64 deep strict签名通过。Rust-only门禁仍未通过；没有实机操作或DMG。详见[Iteration 135](../../iteration-135-progress.md)。

P2B **+0.025人周**至**4.475/8.0（55.94%）**；整体为**12.945/47.5≈27.3%（±3个百分点）**。Go公开状态与更高层call-mode orchestration仍在迁移范围内。


## Iteration 136：模块语音运行时代次分配权迁入 Rust

Rust 私有认证接口现为当前 Go producer 分配运行时代次，Go 仅缓存 Rust 签发的令牌。离线全量测试819通过/3忽略；对应进度与构建见[Iteration 136](../../iteration-136-progress.md)。

## Iteration 137：IMS/VoLTE 就绪业务判定迁入 Rust

Rust 私有 `voice_facts` 投影现在拥有 IMS/VoLTE 就绪 predicate。它仅接受有效 voice tuple，拒绝 USB、运行时下载、探测等无关事实且不执行 I/O；Go 的来电准备、重启后确认和备份还原状态消费该 Rust 结果，投影不可用时失败关闭。全量离线验证 Rust 820通过/3忽略、Go race/vet通过、42项契约工具自测通过；清单276条（Rust195/Go81）、127个Swift调用点、224个审阅范围。Release `Switch.app` 2.89/build289 deep strict签名通过。Rust-only路由门禁仍报告Go fallback及150条Go所有权/依赖路由；未操作模块/读卡器/SIM，也未生成DMG。

P2B增加0.025计划人周等值至4.525/8.0（56.56%）；全项目为12.995/47.5≈27.4%（误差约±3个百分点）。见[Iteration 137](../../iteration-137-progress.md)。


## Iteration 138：模块通话接口就绪判断迁入 Rust

Rust 私有投影新增 `usb_interfaces` 与 `call_interfaces` 阶段，拥有 ADB/UAC 门禁及 ADB+UAC+IMS/VoLTE 的最终 readiness 合取判定。Go 保持 USB 先行、只在 ADB/UAC 就绪后读取语音配置的 I/O 顺序，并消费 Rust 返回的组合结果；既有 IMS 未就绪错误文案和失败关闭行为保留。离线验证 Rust workspace 822通过/3忽略、Go race/vet通过、42项契约自测通过；清单276条（Rust195/Go81）、127个Swift调用点、224个审阅范围。Release `Switch.app` 2.90/build290 deep strict签名通过。Go-only门禁仍报告fallback及150条Go owner/dependency路由；未操作模块/读卡器/SIM，也未生成DMG。

P2B增加0.025计划人周等值至4.550/8.0（56.88%）；全项目为13.020/47.5≈27.4%（误差约±3个百分点）。见[Iteration 138](../../iteration-138-progress.md)。


## Iteration 139：公开模块重启入口迁入 Rust

`POST /api/network/reboot-module` 现在由 Rust `module_radio` 直接处理，并在射频预约、飞行模式/业务状态门禁下执行。响应成功码和字段以及旧 coded-error JSON 保持兼容；Go mux与旧公开 handler移除。离线验证 Rust workspace 全量测试 **1,308通过、0失败、14忽略**（daemon 826通过、3忽略）、Go race/vet、42项迁移契约自测、普通census、Rustfmt及diff检查通过。当前276条契约（Rust196/Go80，含30个HEAD alias）、127个Swift调用点、224个审阅范围；Rust-only门禁仍报告默认Go fallback和149条Go所有权/依赖路由。Release `Switch.app` 2.91/build291 arm64 deep strict签名通过；没有实际模块重启、SIM操作或DMG。

P2B **+0.025人周**至**4.575/8.0（57.19%）**；总体 **13.045/47.5≈27.5%（±3个百分点）**。详情见[Iteration 139](../../iteration-139-progress.md)。

## Iteration 140：USBNet 模式接口迁入 Rust

`POST /api/network/usbnet` 现在由 Rust `module_radio` 执行，Go mux和handler移除。Rust只接受模式0/1，固定生成QCFG命令，在共享射频reservation/物理队列中以8秒命令预算执行，并确保成功、失败路径都尝试释放reservation；保留旧成功字段、400/502错误响应和1 MiB请求体上限。新增参数/动作/释放用例。

此轮还补齐旧 Go ServeMux 冻结方法样本，覆盖以前被遗漏的 Go 路由迁移项；Go生成的HTTP/HEAD响应与Rust对照 **350个wire case通过**、12个原样回落 case通过。完整迁移门禁通过：daemon 830通过/3忽略，Go全仓测试/vet与主包race通过，载波回放2722项以及USB状态、射频投影和PCMA/SDP/AMR对照通过；42项迁移脚本自测及census通过。276条接口为Rust197/Go79，127个Swift调用点，224个review范围。Release `Switch.app` **2.92/build292 arm64** deep strict签名通过；switchd-rs SHA-256 `db834cff70ad84ff22f2b344ebec887bb1983eabf284c8b9986d5f9c930951c5`。Rust-only门禁仍被默认Go fallback和148条Go所有权/依赖路由阻断。没有实际QCFG、模块、读卡器、SIM操作或DMG。

P2B **+0.025人周**至**4.600/8.0（57.50%）**；总体 **13.070/47.5≈27.5%（±3个百分点）**。详情见[Iteration 140](../../iteration-140-progress.md)。

## Iteration 141：公开网络诊断接口迁入 Rust

`GET /api/network` 的主机网络接口、默认路由、DJI USB 观察与五条固定只读 AT 查询现由 Rust 完成。AT 查询串行经过 `module_radio` actor，在一次 Diagnostic reservation 中执行并共享20秒期限；主机命令各自有输出上限和5秒期限。原Go mux注册、handler、PDP DTO/解析器已删除。Rust保留响应字段、原始命令结果和逐项错误，同时提供部分完成数、耗时与采集时间；安全事件仅记录阶段和粗粒度状态，不记录命令输出或主机细节。Go的其他可取消锁等待统一改用新 helper，Go race测试通过。

迁移门禁：Rust workspace **1,318通过、0失败、14忽略**；Go全仓测试与 `go vet` 通过；载波 **2,722** 项、USB状态、射频和codec parity通过；Go/Rust HTTP与HEAD逐字节对照 **359** 个原生案例、**12** 个回落案例通过；迁移工具自测 **42** 项、契约扫描通过。当前277条接口（Rust199/Go78，31个HEAD alias）、127个Swift调用点、225个审阅范围。Release `Switch.app` **2.93/build293 arm64** deep/strict签名通过；`switchd-rs` SHA-256 `6f3dd6128d1f595adc3bc3a74acad503ec004f734a9a8e04cabcea5b21330030`。没有实际AT命令、模块/读卡器/SIM操作或DMG。

P2B **+0.025人周**至**4.625/8.0（57.81%）**；总体 **13.095/47.5≈27.6%（±3个百分点）**。Rust-only门禁仍因默认Go fallback和147条Go所有权/依赖路由失败；离线测试及构建不代表实机网络诊断验收。详情见[Iteration 141](../../iteration-141-progress.md)。

## Iteration 142：网络服务与流量接口移除 Go health 依赖

`GET /api/network/services` 和 `GET /api/network/traffic` 现在从 Rust USB AT owner 读取活动设备描述；没有活动句柄时，使用本地有界 `ioreg` DJI 设备发现，不再请求 Go `/api/health`。保留网络服务 `module` 标记、无模块流量错误和设备字段兼容。此变更不打开额外 USB 句柄、不发送 AT、不改动模块或 SIM。

迁移门禁通过：`proletariusd` **836通过、3忽略**，Go全仓测试和 `go vet`、Rust workspace、网络路由/HTTP HEAD parity及codec/射频兼容检查通过；迁移工具自测 **42项**与契约清单检查通过。277条method/path（Rust199/Go78，31个HEAD alias）、127个Swift调用点、225个review scope。Rust-only门禁现仍被默认Go fallback和**143条**Go owner/dependency路由阻断。Release `Switch.app` **2.93/build293 arm64** deep/strict签名通过，`switchd-rs` SHA-256 `b7e6347054935fd2a814dd5b6509d74e0092f34ca88a49b3a4eb9f91aa86a4d5`。未做实体模块/读卡器/SIM验收，未生成DMG。

P2A **+0.025人周**至**3.760/5.5（68.36%）**；总量 **13.120/47.5≈27.6%（±3个百分点）**。Rust-only构建、Go退出和设备/长稳验收仍未完成。详情见[Iteration 142](../../iteration-142-progress.md)。

## Iteration 143：模块 Profile 备注编解码迁入 Rust

模块 Profile 备注的 CPBR 条目解析、URL-safe unpadded Base64 编解码、ICCID/重复记录处理、Unicode 长度与 255 字节记录上限现由私有 Rust codec 执行。Go 继续拥有公开备注 API、读卡器 AT 命令和硬件事务；Rust 不直接访问设备。错误信息、Go JSON 对无效 UTF-8 的替代行为和旧备注格式保持兼容。

Rust workspace **843通过、3忽略**；`scripts/test-migration-gates.sh`、Go 全仓测试/vet、42项迁移契约工具自测和 `--diff` 清单检查通过。当前279条method/path（Rust201/Go78，31个HEAD alias）、127个Swift调用点、226个审阅范围。Rust-only门禁仍因默认Go fallback及143条Go所有权/依赖路由失败。Release `Switch.app` **2.93/build293 arm64** deep/strict签名通过；内嵌 `switchd-rs` SHA-256 `8813532b694a3e4d46f559784566dfcecd4a6a5eac0c7e83f96a724f04b30841`。没有实体硬件操作或DMG。

P2B **+0.025计划人周等值**至**4.650/8.0（58.13%）**；总量 **13.145/47.5≈27.7%（±3个百分点）**。Rust-only构建、Go退出及设备/长稳验收仍未完成。详情见[Iteration 143](../../iteration-143-progress.md)。


## Iteration 145：模块 Profile 备注公开 API 与 ME 事务迁入 Rust

`GET/PUT /api/esim/module-notes` 现由共享 Rust radio actor 执行，旧 Go handlers/ME AT事务和中间codec RPC移除。读取按32槽批量进行；保存、更新、删除共享操作reservation并对未知写入结果禁止重放。完整 Rust/Go/HTTP parity门禁通过，契约更新为278条（Rust202/Go76，32条HEAD alias）、127个Swift调用点和226个review scope。Rust-only退出门禁仍因默认Go fallback及141条Go所有权/依赖route受阻；实机和最终Go-free验收未完成。详见[Iteration 145](../../iteration-145-progress.md)。

## Iteration 146：eSIM 电话簿探测迁入 Rust

`POST /api/esim/phonebook/probe` 由共享 Rust module-radio actor 接管；旧 Go handler/helper 删除。固定 AT 探测顺序、原 JSON/部分响应和 busy 语义保持不变，未确认的物理结果会停止后续命令。全迁移门禁通过：Rust workspace855通过/3忽略，Go全仓测试/vet及carrier、USB状态、radio policy、HTTP/HEAD和codec parity通过；42项工具自测与契约 `--diff` 通过。契约278条（Rust203/Go75、32个HEAD alias），127个Swift调用点、226个review scope。Rust-only门禁仍因默认Go fallback及140条Go所有权/依赖路由失败。来电按钮修正已通过Release构建与deep/strict签名验证；无硬件操作、未生成DMG。详情见[Iteration 146](../../iteration-146-progress.md)。

## Iteration 148：读卡器列表 Go 依赖审计补正

复核确认 Rust `GET /api/reader/list` 在 PC/SC 扫描后仍调用 Go 私有 `POST /api/internal/reader/reconcile` 获取 Go-owned 选卡与 BLE 状态；契约清单此前漏记该依赖。现已登记该依赖，HEAD alias继承它，并加入扫描器回归测试。43项契约工具自测、279条路由清单一致性检查通过；Rust-only退出门禁仍因默认 Go fallback 与141条 Go owner/dependency 路由阻断。本轮未改变生产 handler owner，不计迁移分，也未访问硬件。详情见[Iteration 148](../../iteration-148-progress.md)。

## Iteration 149：USB/PC/SC eSIM 健康读取迁入 Rust

选定 USB/PC/SC reader 的 `GET /api/reader/esim/health` 现由 Rust 读取 Profile 健康信息，并保留旧响应投影。HTTP 等待取消不会提前释放 PC/SC lease；worker 持有 lease，直到 APDU 事务结束且释放得到确认。reader manager generation lease/admission、完整 operation 投影、BLE/无 PC/SC及兼容回退仍依赖 Go。

仅已知 ISD-R AID 的 `6A82` 走既有分类 fallback；其他 APDU 错误失败关闭，`6982` 不重试、不绕过鉴权，9eSIM `6982` 仍排除。模块 Profile 备注的不确定写入也加了 fence，成功读取核对前拒绝再次写入，错误内容不回显 AT 记录。

Rust workspace 1,357通过/14忽略；Go全仓 `go test -timeout=30s ./...` 和 `go vet ./...` 通过；冻结 Go HTTP GET/HEAD parity、格式检查、契约 `--diff` 及43项工具自测通过。契约280条（Rust207/Go73，34个HEAD alias）、127个Swift调用点、237个review scope。Rust-only门禁仍因默认Go fallback与142条Go owner/dependency路由失败。

Release `Switch.app` 2.93/build293 arm64 构建及deep/strict签名通过；`switchd-rs` SHA-256 `0157ca248b6ff52c8b4b188c7198a85263f16238340fe7cf910ef6a23381cee6`。未操作实卡、未生成DMG。P2B +0.025计划人周至4.750/8.0（59.38%）；总体13.245/47.5≈27.9%。详情见[Iteration 149](../../iteration-149-progress.md)。

## Iteration 151：选定 USB/PCSC 读卡器 AKA 入口接入 Rust

选定且已就绪的 USB/PCSC reader 调用 `POST /api/reader/auth` 时，Rust 使用 generation-bound reader lease 和既有 PC/SC transaction 完成一次 AKA；RAND/AUTN/mode 校验顺序、成功字段与错误投影保持兼容，挑战材料清零。BLE、未选卡及提交前无法取得租约时仍调用 Go 兼容路径；APDU 提交后不回退、不重放，租约覆盖完整事务与确认释放。Go generation/session manager、BLE以及完整 VoWiFi 生命周期仍未迁移，因此不能称作端到端 Rust AKA 闭环。同一迭代还将IMS外发短信的MESSAGE构造、Call-ID/CSeq匹配、多段接受状态和超时unknown保护接入Rust；Go仍持有现有IMS TCP连接、SIP事件派发和会话生命周期，Rust私有UDS返回的只是待发送帧与状态，并不证明运营商已投递。

来电页呼入状态的接听/挂断按钮改为以通话卡片可用宽度为基准整组居中，固定按钮自然宽度，避免宽内容容器下偏左。Debug `Switch.app` 2.93/build293 编译通过，产物在 `../build/DerivedDataCallActionsFix/Build/Products/Debug/Switch.app`；未生成DMG。

`scripts/test-migration-gates.sh` 全量门禁通过：Rust workspace **890通过、3忽略**；Go全仓测试/vet、载波及USB/射频/codec parity、冻结HTTP/HEAD差分、43项契约工具自测及契约 `--diff` 通过。清单281条（Rust209/Go72，34个HEAD alias）、127个Swift调用点、248个审阅范围。Rust-only门禁仍因默认Go fallback和143条Go owner/dependency路由失败。最近一次App Debug构建早于IMS短信事务变更，本轮未重建App。未连接读卡器/实体SIM，未发送短信或访问运营商网络；9eSIM `6982` 未处理，未生成DMG。

本轮P2B与P3各计入 **+0.025计划人周**；总进度 **13.295/47.5 = 28.0%**（约±3个百分点），P2B **4.775/8.0 = 59.69%**，P3 **1.465/8.0 = 18.31%**。该切片仍保留Go租约与兼容回退；纯UI布局不计入迁移比例。

## Iteration 152：BLE 热插拔确认状态迁入 Rust

Rust 私有 reader-hotplug owner 承担 token/generation/observation-ID fencing 和连续 BLE 缺席确认；Go 仍提供 BLE inventory、在最终断开前复核 native lease 和 authMu、停止 VoWiFi 并清理 N 待代次。失败或不确定的 RPC 不会触发断开。该切片只迁移确认状态，不接管设备发现或 teardown。

全量 `scripts/test-migration-gates.sh` 通过；Rust workspace **1,389通过、6忽略**（proletariusd 896通过、3忽略），Go 全仓测试/vet、载波2,722项、USB/射频/codec与HTTP/HEAD parity通过；43项契约工具自测、契约 `--diff` 与 Rustfmt 通过。最新清单283条（Rust211/Go72，含34个HEAD alias）、127个Swift调用点、250个审阅范围。Rust-only门禁仍因默认Go fallback与**145条**Go owner/dependency路由失败。最近一次 `Switch.app` 2.93/build293 构建早于本轮后端变更，本轮未重建App。未连接实卡或操作读卡器，也未生成DMG。

P2B **+0.025计划人周**至**4.800/8.0（60.0%）**；总体 **13.320/47.5≈28.0%**（约±3个百分点）。UI修正不计入后端迁移进度。详情见[Iteration 152](../../iteration-152-progress.md)。

## Iteration 153：Rust 接管 N待入站事件归一化与 IMS 身份存储

Go N待 IMS回调现在只把收到的原始 RPDU/TPDU、解码结果、时间戳、状态报告和拼接元数据发送给私有 Rust store action。Rust按旧版 `multiID("mt", profile, RPDU)` 规则计算相同的 profile-scoped 去重ID，并完成耐久归档、multipart冲突隔离/安全回退和状态报告关联；回归覆盖网络重试、跨Profile隔离、拼接完成及冲突不误合并。Go IMS socket和事件确认生命周期仍未迁移。

Rust reader-device-identity owner同时负责兼容 `imei.json` 格式、原子私有写入及锁内首次随机生成。内部 resolver 在main dispatcher完成私有token校验，shutdown期间拒绝新建；Go VoWiFi仅在每次IMS session启动时请求当前身份，RPC失败返回空并停止继续注册。旧Go文件生成器仍保留在显式Go-only兼容路径。

全量 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,394通过、6忽略**（proletariusd 901通过、3忽略）；Go全仓vet/test、载波2,722项、USB状态/射频/codec/HTTP HEAD parity通过。契约 `--diff`、43项工具自测和Rustfmt通过。清单285条（Rust213/Go72，34条HEAD alias）、127个Swift调用点、251个审阅范围。Rust-only门禁仍因默认Go fallback与145条Go owner/dependency路由失败。未构建App、未生成DMG，未操作读卡器或SIM。

P2B **+0.025计划人周**至**4.825/8.0（60.31%）**，P4 **+0.025**至**0.475/4.5（10.56%）**；总体 **13.370/47.5≈28.1%**（约±3个百分点）。这不代表VoWiFi首次身份生成已实卡验收，也不代表N待网络持续恢复或多Profile仲裁已完成。详情见[Iteration 153](../../iteration-153-progress.md)。

## Iteration 154：入站短信 RP-ACK delivery-report 帧迁入 Rust

入站 SIP MESSAGE 仍由 Go 的已注册 IMS transport 处理：先立即返回 SIP 200，再解析 RP-MT-DATA 并调用当前耐久入库回调。回调成功后，Go 通过原认证 UDS transaction route 请求 Rust 构造单独的 delivery-report SIP MESSAGE。Rust生成 Call-ID、branch/tag、headers 与二进制 RP-ACK `[0x02, reference]` body，并在同一 IMS SMS session 下分配高于现有 floor 的 CSeq；Go把返回帧写入当前注册 TCP stream 一次，并在 Go event loop 中等待匹配的 SIP final response。旧路由、字段和 SIP/RP wire 语义保持不变，9eSIM `6982` 不在本轮处理范围。

这次迁移只把 delivery-report 帧与 CSeq 构造交给 Rust。入站 SIP 200、耐久回调/是否确认的决策、注册 socket/TCP stream、SIP 响应接收/匹配、Go session 生命周期、IMS 注册/VoWiFi/IKE、掉线恢复和真实运营商验收仍由 Go 或现有层负责；Rust actor不持有delivery-report的live response event。

`scripts/test-migration-gates.sh` 完整离线门禁通过：Rust workspace 1,398 passed、6 ignored（`switchd-rs` 903 passed、3 ignored）；Go全仓 vet/test通过；carrier replay 2,722 cases；USB status、radio、HTTP/HEAD、PCMA fixture、SDP与AMR parity通过。Rust migration contract `--diff`/一致性及43项工具自测通过，清单仍为285条（Rust213/Go72）、127个Swift调用点、251个审阅范围。Rust-only退出门禁仍被145条Go owner/dependency路径和默认fallback阻断。未构建App/DMG、未接读卡器/SIM或访问运营商网络。

P3按计划权重 +0.025人周至1.490/8.0（18.63%）；累计13.395/47.5人周（28.2%，估算误差约±3个百分点）。详情见[Iteration 154](../../iteration-154-progress.md)。

## Iteration 155：BLE eSIM 健康读取的 APDU 与 Profile 扫描迁入 Rust

选中的 BLE 读卡器现与 USB/PCSC 共用 Rust eSIM health/profile scanner。Rust 通过有界、鉴权的 Unix socket 调用现有 Swift CoreBluetooth bridge；每条 APDU 都携带预期 BLE reader identity，由 bridge 在调用 CoreBluetooth 前重新校验连接身份。Rust 一次只提交一个 APDU，不重试；APDU worker 持有既有 Go generation-bound reader-session lease 与 heartbeat，发现租约丢失后不再提交后续 APDU。桥接状态/配置未就绪且尚未发送 APDU时保留原 Go 兼容响应；已提交 APDU 的失败继续关闭处理。只将已知 ISD-R `6A82` 视为候选 AID 不存在；`6982` 不重试、不绕过鉴权，也不处理此前明确排除的 9eSIM 下载问题。

Rust workspace、Rustfmt、离线 fake-UDS/health-scan、契约 --diff 和工具自测证据见[Iteration 155](../../iteration-155-progress.md)。本轮新增2个reader BLE APDU私有action contract；当前清单287条（Rust216/Go71，含35个HEAD alias）、127个Swift HTTP调用点、254个审阅范围。当前迭代未改Go源码，未重跑Go全仓测试；USB-status/radio/HTTP方法差分fixture沿用其最近一次完整门禁记录。Go 仍负责 reader selection、generation/session admission/release、完整 BLE 生命周期和其他 BLE LPA 操作，所以这不是端到端 Go-free。未连接实卡或向硬件执行 APDU，也未生成DMG。

本轮按计划权重计 P2B **+0.025人周**至**4.850/8.0（60.63%）**；整体 **13.420/47.5≈28.3%**（约±3个百分点）。该权重只代表本切片的 Rust 语义迁移估算，并不代表设备验收。

## P2A：移除未引用的 Go 短信转发执行器

`backend/internal/smsforward` 无生产 Go 导入；Rust 已拥有转发状态、队列、provider 投递与 retry/unknown 策略，因此删除了重复 Go 执行器、控制器启动及五个兼容 fallback 注册。Rust route 和 Swift 调用保持不变，这项退役不计作生产职责迁移。保留历史 Go SWF1 fixture 的 Rust 兼容回归，以及仅验证共享 flock 的最小 Go interop peer。Rust 慢 provider 并发测试覆盖网络请求阻塞期间仍可查询/入队、在途任务不可取消、并发 flush 不重复投递。

P2A 独立分支的完整功能门禁、contract/HTTP 集成和 Swift suite 已通过；集成后清单刷新为 **280** 个 method/path contract（Rust **208** / Go **72**，含34个HEAD alias）、127个Swift HTTP调用点及237个审阅范围。该数值只反映路由扫描；reader/auth 与其他 Rust handler 仍有 Go compatibility/硬件依赖，Go-free 构建和实机验收尚未完成。本项不增加迁移完成率，也没有生成DMG或访问实体设备。

详情见[P2A执行器退役记录](p2a-forwarding-go-runtime-retirement.md)与[P2A旧路由切换说明](p2a-forwarding-legacy-go-cutover.md)。

## Iteration 156：IMS 入站短信解析与 BLE 操作入口迁入 Rust

Rust现解析IMS入站RPDU/TPDU并组装统一短信事件，覆盖普通短信、分段字段、状态报告和畸形/不支持内容的原始留存；Go侧解码器与回退路径移除。Go仍管理IMS SIP/socket/session事件顺序和持久化回调，并只在持久化成功后发送RP-ACK。BLE scan/connect/disconnect公开处理器迁到Rust，调用认证Go私有读卡器租约路由以共享硬件仲裁，再由Swift CoreBluetooth桥访问BLE设备，因此仍有明确Go依赖。

`scripts/test-migration-gates.sh`全量门禁通过：Rust workspace **1,428通过、6忽略**（`proletariusd` **929通过、3忽略**）；Go全仓测试/vet、载波2,722项回放、USB状态/射频/codec与HTTP/HEAD parity、43项契约工具自测和迁移契约`--diff`通过。定向Go race测试、Rustfmt和`git diff --check`也通过。最新清单288条（Rust219/Go69，含35个HEAD alias）、127个Swift调用点、257个审阅范围。`--require-rust-only`按预期失败：默认Go fallback仍在，145条路由仍含Go owner/dependency。无App重建、读卡器/SIM操作或DMG。

P2B **+0.025计划人周**至**4.875/8.0（60.94%）**；P3 **+0.025**至**1.515/8.0（18.94%）**；总体 **13.470/47.5≈28.4%**（约±3个百分点）。BLE接口仍通过Go租约，IMS耐久持久化与SIP生命周期仍由Go负责；没有把这些部分计算为已完成。详情见[Iteration 156](../../iteration-156-progress.md)。

## Iteration 157：IMS 入站短信解码与 N 待耐久入库合并到 Rust

新增已认证的内部 IMS transaction action：Rust解码入站RPDU/TPDU、组装短信事件并按profile写入N待耐久存储，在同一事务响应中返回`store_state`。Go的N待回调必须确认收到已持久化结果后才继续原有RP-ACK流程；普通读卡器IMS仍采用decode-only路径。Go不再序列化N待入站事件，也不再单独调用第二个存储RPC。SIP会话、注册流、事件顺序和RP-ACK传输仍由Go管理；未增加HTTP method/path。

Rust transaction测试覆盖首次入库、同profile重试去重、跨profile隔离和畸形RPDU不提交；Rust/Go进程离线集成15项检查通过。完整迁移门禁通过：Rust daemon 930通过/3忽略、Go全仓测试与vet、载波2,722项回放、USB/射频/codec/HTTP parity、迁移契约`--diff`及43项工具自测均通过。当前清单288条（Rust219/Go69，含35个HEAD alias）、127个Swift调用点、257个审阅范围。Rust-only门禁仍因默认Go fallback和145条Go owner/dependency路由而失败。未连接实体读卡器/SIM、未做在线IMS验收、未重建App或生成DMG。

P3 **+0.025计划人周**至**1.540/8.0（19.25%）**；累计**13.495/47.5≈28.4%**（约±3个百分点）。详见[Iteration 157](../../iteration-157-progress.md)。


## Iteration 158：BLE eSIM 操作、普通 IMS 短信归档与 N 待 Profile 投影

选中 BLE reader 的 Profile 改名和启用/切换 APDU 序列现由Rust执行，仍依赖Go generation-bound reader mutation lease/finalizer与Swift CoreBluetooth桥。`GET /api/multistandby/profiles` 的公开列表和offline/desired-state投影迁Rust，实时runtime snapshot仍经认证Go适配器，多Profile session/card arbitration仍由Go负责。普通reader/module IMS 短信现在由Rust完成解码和既有归档提交，Go只在Rust确认归档后继续RP-ACK；SIP会话/注册流仍由Go负责。

全量迁移门禁通过：Rust workspace（daemon 937通过/3忽略）、Go全仓test/vet、2,722项carrier replay、USB/radio/HTTP-HEAD parity、43项契约自测、迁移契约`--diff`及Rustfmt通过；真实Rust supervisor + Go子进程集成16项通过。清单290条（Rust221/Go69，36个HEAD alias）、127个Swift HTTP调用点、265个审阅范围。Rust-only退出检查仍被默认Go fallback和145条Go owner/dependency route阻断。未连接设备/运营商、未构建App、未生成DMG。

Iteration 158计入P2A和P2B各+0.025计划人周：P2A为3.785/5.5（68.82%），P2B为4.900/8.0（61.25%）；P3维持1.540/8.0（19.25%），P4维持0.475/4.5（10.56%）。总体13.545/47.5≈28.5%（估算误差约±3个百分点）。详见[Iteration 158](../../iteration-158-progress.md)。

## Iteration 159：BLE Profile 删除与 N 待卡片列表投影迁入 Rust

选中 BLE reader 的 Profile 删除现在与 USB/PCSC 共用同一 Rust LPA 删除、通知确认及 NVRAM 统计逻辑。Go 仍负责 generation-bound mutation lease、VoWiFi协调和最终释放；Swift CoreBluetooth仍是 APDU系统桥。租约授权后再次校验当前选中 reader，防止设备切换后误操作。未新增HTTP method/path。

`GET /api/multistandby/cards` 现在由 Rust分组投影；Go删除公开处理器，改为只提供认证的内部 runtime-snapshot，刷新物理清单并返回易变 reader/EID/可用性事实。`GET /api/multistandby/profiles`保持Rust catalog/desired-state投影。卡片和Profile实时会话、物理发现与多Profile仲裁仍由Go管理。

全量 `scripts/test-migration-gates.sh` 通过：Rust workspace中 `proletariusd` **937通过、3忽略**，Go全仓测试与`go vet`、2,722项carrier replay、USB/radio/HTTP-HEAD/codec parity均通过。新fake BLE删除测试驱动了完整删除APDU序列，并核对每条命令绑定选中reader及token；Rust/Go隔离进程集成 **17项**通过，包含卡片列表、Profile投影和普通IMS短信归档。迁移契约扫描与`--diff`通过；工具自测43项通过，清单291条（Rust223/Go68，37个HEAD alias）、127个Swift HTTP调用点、268个审阅范围。Rust-only门禁仍因默认Go fallback及147条Go owner/dependency route失败。另修复了卡片投影测试的显式类型推断构建错误。

保守计入P2B **+0.025计划人周**至**4.925/8.0（61.56%）**；总量从13.545增至**13.570/47.5≈28.6%**（估算误差约±3个百分点），待完成约71.4%。 P2A为3.785/5.5，P3为1.540/8.0，P4为0.475/4.5。未构建App或DMG，未连接实体读卡器/SIM、未开展真实IMS/运营商验证。详情见[Iteration 159](../../iteration-159-progress.md)。
## Iteration 160：BLE 通知读取与手动重试迁入 Rust

选中BLE reader的通知列表和手动重试现在使用Rust的共用LPA transport实现，并通过Swift桥将APDU绑定到选中reader UUID。Go仍保留generation-bound lease、heartbeat/finalizer、选择复核和缓存；无选中reader时保留旧Go兼容路径。

完整迁移门禁通过：Rust daemon 939通过/3忽略，Go全仓test/vet、2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity通过。Rust/Go隔离进程集成17项、迁移契约工具43项、`--diff`、Rustfmt与`git diff --check`通过。两个fake BLE测试分别验证空通知列表、缺失序号和APDU的UUID/token绑定；没有真实ES9+投递或实体设备验收。契约291条（Rust223/Go68，含37个HEAD alias）、127个Swift HTTP调用点、270个审阅范围。Rust-only门禁仍因默认Go fallback及147条Go owner/dependency route失败。未构建App或DMG。

保守计入eSIM/LPA **+0.025计划人周**，由1.100/5.5提升至**1.125/5.5（20.45%）**；总体由13.570增至**13.595/47.5≈28.6%**（误差约±3个百分点）。P2A 3.785/5.5、P2B 4.925/8.0、P3 1.540/8.0、P4 0.475/4.5维持不变。详见[Iteration 160](../../iteration-160-progress.md)。

## Iteration 161：VoWiFi 代理诊断路由迁入 Rust

`POST /api/reader/vowifi/proxy-check`现在由Rust处理，复用既有SOCKS5握手探测与SWU传输选择器；出口IP归属查询是4秒超时、64KiB响应上限的可选请求，失败不改变本地代理检测结论。保持旧响应字段，并明确不把本地握手/配置检查表述成ePDG UDP往返、IKE或IMS已就绪。Go公开handler及重复探测代码已删除；无新增HTTP契约。

全量迁移门禁通过：Rust daemon 946通过/3忽略，Go全仓测试/vet、2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity通过；Rust/Go进程集成18项通过（含旧空代理响应），定向Rust代理测试6项与并发ID唯一性测试通过、Go route/method parity、契约工具43项、`--diff`及Rustfmt通过。契约291条（Rust224/Go67，含37个HEAD alias）、127个Swift HTTP调用点、272个审阅范围。Rust-only门禁仍因默认Go fallback和147条Go owner/dependency route阻断。没有真实代理网络、ePDG、IMS或SIM测试，未制作App/DMG。

保守计入P2A **+0.025计划人周**，由3.785/5.5升至**3.810/5.5（69.27%）**；总量由13.595升至**13.620/47.5≈28.7%**（估算误差约±3个百分点）。P3维持1.540/8.0（19.25%）；其他阶段本轮不计分。详见[Iteration 161](../../iteration-161-progress.md)。

## Iteration 162：IMS OPTIONS事务校验与短信转发数据源解耦

Rust现在校验IMS OPTIONS最终响应与本次Call-ID、CSeq、方法是否一致；Go若没有Rust校验通道则失败关闭。心跳定时器、SIP socket和响应流仍由Go管理。短信转发producer直接合并读取Rust持久归档和USB预览，不再依赖Go子进程generation或重复volatile快照；部分IMS/模块短信仍从Go兼容会话进入持久归档，因此不代表实时IMS收件端已Rust化。

全量迁移门禁通过：Rust daemon 947通过/3忽略、Go全仓test/vet、2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity；18项跨进程集成、43项契约自测、`--diff`、Rustfmt与`git diff --check`通过。清单291条（Rust224/Go67，37个HEAD alias）、127个Swift HTTP调用点、272个审阅范围。Rust-only门禁仍因默认Go fallback与147条Go owner/dependency route受阻。未操作硬件/SIM或真实网络，未构建App/DMG。

P2A增至**3.835/5.5（69.73%）**，P3增至**1.565/8.0（19.56%）**；两项各增加0.025计划人周等值，总体**13.670/47.5≈28.8%**（估算误差约±3个百分点）。Go-free构建/运行、IMS session退出及实机验收仍未完成。详见[Iteration 162](../../iteration-162-progress.md)。

## Iteration 163：转发空账本恢复与跨进程未知结果防重放

首次启用短信转发时，无变化的空准入状态不再落盘为会被后续视为损坏的`[]`；既有合法空账本会安全归一化。新增loopback合成Bark进程测试：provider已收到请求、Rust尚未收到确认时强杀daemon，重启后任务进入`paused_result_unknown`，完整轮询周期不自动重放；显式重试才发出第二次请求并完成。非空账本和损坏账本仍按原规则保护。

本轮 `proletarius-store` 8项、Rust daemon 951项通过/3项忽略，19项Rust/Go隔离进程集成、契约`--diff`、Rustfmt和差异检查通过。Iteration162仍是最近一次完整迁移门禁；本轮没有重跑全套Go/carrier/parity。真实provider投递、系统PAC、设备及SIM验收尚未完成，也未生成DMG。

P2A增至**3.860/5.5（70.18%）**；总进度为**13.695/47.5≈28.8%**（估算误差约±3个百分点）。Go-free构建/运行和全部端到端验收仍未完成。详情见[Iteration 163](../../iteration-163-progress.md)。

## Iteration 164：BLE eSIM 概览读取与 PKI 资源归属迁入 Rust

选中 BLE reader 时，Rust 经现有 Swift CoreBluetooth/APDU bridge 读取 EID、eUICC信息/地址、Profile列表和可选eSTK产品字段；既有 JSON 投影与PKI issuer/accreditation识别保持兼容。PKI JSON 由Rust `proletarius-euicc` crate自有资源嵌入，与Go兼容副本校验一致。Go仍控制generation-bound reader lease/heartbeat/finalizer、操作快照和其他设备会话；未选中BLE或不适用传输仍保留兼容路径。

全量迁移门禁通过：Rust workspace daemon 951通过/3忽略，Go全仓test/vet、2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity通过；Rust/Go隔离进程集成19项，契约工具43项和`--diff`通过。契约清单291条（Rust224/Go67，37个HEAD alias）、127个Swift调用点和272个review scopes。无实卡/APDU运营商验收，9eSIM `6982`仍排除，也未构建DMG。

P2B **+0.025计划人周**至**4.950/8.0（61.88%）**；累计**13.720/47.5≈28.9%**。详情见[Iteration 164](../../iteration-164-progress.md)。

## Iteration 165：短信转发生产者触发链路端到端覆盖

21项Rust/Go隔离进程集成中，新增两条合成事件流：Go兼容IMS coordinator写入N待耐久store后自动转发；Go生产`archiveSMSChecked`把设备短信写入Rust archive后自动转发。两类事件由Rust producer进入同一个loopback Bark端点，各接收一次，额外观察完整轮询周期无重复。fixture调用真实生产归档客户端和Rust认证UDS，不接触硬件，也不增加生产HTTP API。

Iteration164是最近一次完整migration-gates证据；本轮重建Rust daemon与Go fixture，21项跨进程检查和契约`--diff`通过，未重跑全套Go/carrier/parity。PAC、真实provider/设备与运营商验收仍未完成。

P2A **+0.025计划人周**至**3.885/5.5（70.64%）**；总进度 **13.745/47.5≈28.9%**（估算误差约±3个百分点）。详见[Iteration 165](../../iteration-165-progress.md)。

## Iteration 166：N待操作账本迁入 Rust

Rust持久管理N待start/stop/retry请求指纹、幂等键、操作状态及重启未知结果隔离；Go执行器开始设备/会话操作前必须取得Rust持久化单次claim，迟到/并发重复调用无法再次启动。执行结果未知不自动重放，Go仍负责真实Profile会话和物理卡生命周期。全量迁移门禁通过：Rust daemon 958通过/3忽略、Go全仓test/vet、2,722项carrier replay及USB/radio/codec/HTTP-HEAD parity；22项跨进程集成和8项定向操作账本测试通过。清单295条（Rust231/Go64，含38个HEAD alias）、127个Swift调用点、278个审阅范围。Rust-only仍有默认Go fallback及149条Go owner/dependency路由，未做实卡或DMG。

P4 **+0.025计划人周**至**0.500/4.5（11.11%）**；总进度**13.770/47.5≈29.0%**（估算误差约±3个百分点）。详情见[Iteration 166](../../iteration-166-progress.md)。

## Iteration 167：N待操作唯一执行领取迁入 Rust

Go不再保存进程内operation ID去重表；在Go校验当前卡片/Profile后，先向Rust耐久账本领取执行权。Rust原子地把pending转为running，并发或重复回调只有一个赢家；重启后的running转为unknown且不可再领取。停止期间claim被拒绝，终态transition仍可完成。Go继续执行设备/Profile会话，操作执行并未Go-free。

完整迁移门禁通过：Rust workspace 1,456通过/6忽略（daemon 959通过/3忽略），Go全仓test/vet、2,722项载波回放、USB/radio/codec/HTTP-HEAD parity通过；22项Rust/Go进程集成和8项定向Rust操作账本测试通过。契约295条（Rust231/Go64，38个HEAD alias）、127个Swift调用点、278个审阅范围。Rust-only门禁仍受默认Go fallback及149条Go owner/dependency路由阻断；未做实卡或DMG。

P4 **+0.025计划人周**至**0.525/4.5（11.67%）**；总进度**13.795/47.5≈29.0%**（误差约±3个百分点）。详情见[Iteration 167](../../iteration-167-progress.md)。


## Iteration 168：N待恢复退避策略决策迁入 Rust

Rust 私有 recovery-plan RPC 负责稳定在线窗口、连续故障计数、计划续期/重认证延迟、立即人工重试和意外故障退避。Go 只提交有界的计时事实，校验返回值后仍运行 timer 和 Profile 恢复；Rust IPC 失败时保留受限兼容回退。完整迁移门禁通过：Rust workspace 1,459通过/6忽略（daemon 962通过/3忽略），Go全仓test/vet、2,722项载波回放、USB/radio/codec/HTTP-HEAD parity以及23项跨进程集成通过。契约296条（Rust232/Go64，含38个HEAD alias），127个Swift调用点、278个审阅范围；Rust-only仍有默认Go fallback和150条Go owner/dependency路由。未操作实卡、未构建App或DMG。

P4 **+0.025计划人周**至**0.550/4.5（12.22%）**；总进度**13.820/47.5≈29.1%**。详情见[Iteration 168](../../iteration-168-progress.md)。


## Iteration 169：N待逐 Profile 恢复状态持久化到 Rust

Go不再向 recovery-plan RPC 发送本地失败计数或稳定在线影子值；请求改为Profile ID、原因码、观察到的在线时长和观测缺口。Rust按Profile在runtime journal中持久保存失败预算与稳定在线累计时长，并返回规范化原因码。旧journal的缺省字段保持可读，状态上限和持久化失败采取有界、fail-closed处理。Go仍执行计时器、Profile session、物理卡恢复，并保留IPC错误下的有界过渡回退。

全量迁移门禁、23项监督进程集成及契约`--diff`通过。P4增加0.025人周至0.575/4.5（12.78%）；总体增至13.845/47.5≈29.1%。该增量不包括N待session/broker、物理恢复和实机长稳验收。详情见[Iteration 169](../../iteration-169-progress.md)。

## Iteration 170：N待启动恢复轮询与代次去重归 Rust

Rust daemon 现在每 5 秒检查持久 `keep_online` 意图与 Go 后端就绪代次；只有存在意图时才调用现有认证 `runtime-snapshot` 适配器。读卡器不可用时重试，成功后每个 Go 代次只协调一次；Go 子进程或 Rust supervisor 重启会触发新一轮协调。Go启动ticker已删除，但Go仍拥有物理inventory/session启动、broker和逐Profile恢复timer。

Rust workspace 1,463通过/6忽略（daemon 966/3），Go全仓test/vet、26项监督进程集成、2,722项carrier replay及USB/radio/codec/HTTP-HEAD parity通过；契约清单296/127/277，`--diff`和43项工具自测通过。Rust-only出口及实机/长稳验收未完成。P4至0.600/4.5（13.33%），总进度13.870/47.5（29.2%）。详见[Iteration 170](../../iteration-170-progress.md)。

## Iteration 171：N待运行观察事实归一化迁入 Rust

Go只向私有 observation RPC 发送有界的时间、Profile ID、代次、原因码、`desired` 以及 tunnel、IMS、messaging、SIM短信路由就绪事实。Rust白名单归一化原因并推导规范状态与恢复标记，继续写入既有 Event journal 格式；公开报告及旧 journal 格式保持兼容。Go仍负责采集实体 IMS/隧道/SMS路由就绪事实与 Profile session。

Rust workspace 1,464通过/6忽略（daemon 967/3），Go全仓test/vet、26项监督进程集成、2,722项carrier replay及USB/radio/codec/HTTP-HEAD parity通过；契约清单296/127/277，`--diff`和43项工具自测通过。Rust-only出口及实机/长稳验收未完成。P4至0.625/4.5（13.89%），总进度13.895/47.5（29.3%）。详见[Iteration 171](../../iteration-171-progress.md)。


## Iteration 172：N待维护调度与短信入队迁入 Rust

Rust daemon持有N待每秒维护节拍，Go原先每Profile ticker已删除；认证Go adapter只向活动session worker发送可合并信号，Go继续执行flush、就绪采样和session/读卡器生命周期。公开 `POST /api/multistandby/messages/send` 的校验和耐久幂等入队由Rust负责；Go只提供当前reader generation/session/SMS通道的只读就绪事实。

完整迁移门禁通过：Rust workspace与format、Go全仓test/vet、2,722项carrier replay、USB/radio/HTTP method/HEAD、codec/SDP parity；Rust/Go隔离进程集成28项，迁移契约`--diff`及43项自测通过。清单298条（Rust233/Go65，38个HEAD别名）、127个Swift调用点、278个审阅范围。Rust-only退出、N待broker/session、多Profile仲裁、Go-free运行及设备长稳验收仍未完成。未操作实体读卡器/SIM或运营商网络，未构建App/DMG。

P4增加0.050计划人周等值至**0.675/4.5（15.0%）**；总体为**13.945/47.5≈29.4%**（估算误差约±3个百分点）。详情见[Iteration 172](../../iteration-172-progress.md)。

## Iteration 173：N 待逐 Profile 耐久重试时钟迁入 Rust

Rust runtime journal 持久化每个 Profile 的 reconnect backoff deadline、Go boot owner 与 attempt generation；重复请求保留既有deadline，陈旧代次拒绝，Rust dispatcher只在Go ready时投递到期事件。确定性0–999ms抖动分散同时故障的Profile。历史快照中曾由Go保留有界本地timer fallback；Iteration353已移除，当前失败调度会等待显式操作，Go仍负责重连worker与物理/session恢复。

完整门禁通过：Rust workspace/Go全仓test-vet、2,722项carrier replay、USB/radio/HTTP method/HEAD、codec/SDP parity；29项跨进程集成和迁移契约工具验证通过。该轮契约300条（Rust234/Go66，含38个HEAD alias）、127个Swift调用点和277个review scopes。

P4 +0.025计划人周等值至**0.700/4.5（15.6%）**；总进度**13.970/47.5≈29.4%**。详情见[Iteration 173](../../iteration-173-progress.md)。

## Iteration 174：IMS 注册续期 deadline 迁入 Rust

Rust journal将IMS REGISTER lease renewal deadline与N待重连backoff分开耐久保存；注册期限不加抖动以保留lease安全余量。认证到期callback绑定Go boot owner、Profile、desired/session状态与session generation，再经Go现有failure arbitration触发planned renewal。Go仍执行SIP/session与硬件恢复。历史fallback已在Iteration353退役；当前Rust scheduler失败时停止自动恢复，等待手动重试/停止。

完整门禁通过：Rust workspace 1,474通过/6忽略（daemon 975/3），Go全仓test/vet、2,722项carrier replay、USB/radio/HTTP method/HEAD、codec/SDP parity；30项跨进程集成，契约302条（Rust235/Go67，38个HEAD alias）、127个Swift调用点、278个review scopes，`--diff`及43项工具自测通过。Rust-only出口、N待broker/session、多Profile在线仲裁、Go-free运行及设备/长稳验收仍未完成。

P4 +0.025计划人周等值至**0.725/4.5（16.1%）**；总进度**13.995/47.5≈29.5%**。详情见[Iteration 174](../../iteration-174-progress.md)。

## Iteration 175：公开健康状态与 N 待重连代次迁入 Rust

Rust 网关现在直接生成 `/api/health` 的兼容响应，并以同一代次绑定的投影为 `device-snapshot` 提供健康字段。健康事实通过带内部认证的 Go observation adapter 获取；Rust 在调用前后验证 Go 子进程仍处于同一可用代次，并校验请求代次、响应 DTO 与必需字段。Go `GET /api/health` 在显式兼容启动模式中继续工作，因此本轮没有移除 Go 健康采集或 Go 运行依赖。

N待重连调度不再由Go分配generation。Go为每次尝试生成opaque token并提交有界delay，Rust负责单调generation分配以及schedule和counter原子持久化；恢复旧journal时从pending schedule播种counter。同token重复请求幂等、冲突复用拒绝。到期callback同时携带Rust generation与attempt token，Go只有当前desired runtime匹配时才唤醒worker。手动重试后迟到通知只会确认，不会缩短新一轮等待。Rust负责时钟、持久状态和回调栅栏；Go仍负责实际Profile/session恢复，Rust IPC故障时保留有界本地timer。

完整 `scripts/test-migration-gates.sh` 通过；daemon **982 passed/3 ignored**，Go 全仓 `vet`/测试、2,722 项 carrier replay、USB/radio/HTTP method+HEAD/codec+SDP parity 通过。Rust retry journal 21项定向测试和30项Rust/Go跨进程集成覆盖generation分配、旧journal恢复、重复/冲突请求与迟到callback；健康投影覆盖wire-shape、必需字段、代次不匹配和查询期间子进程重启。新增健康路由合同为 `HEAD /api/health` 与私有 `POST /api/internal/runtime/health-snapshot`。当前清单304条（Rust237/Go67，含39个HEAD别名）、127个Swift调用点、284个审阅范围，契约差异和43项检查器自测通过。`--require-rust-only` 仍被默认 Go fallback 和现有 Go owner/dependency 阻断；没有操作读卡器、号卡、短信或运营商网络，也未生成 App/DMG。

P2A健康投影计入 **+0.025 计划人周等值**，达到 **3.910/5.5（71.1%）**；P4重连代次/调度另计 **+0.025**，达到 **0.750/4.5（16.7%）**；总体为 **14.045/47.5≈29.6%**（估算误差约±3个百分点）。Go-free健康发现、设备观测、N待物理/session接管及最终构建验收仍未完成。

## Iteration 409–410 更新

Iteration 409 将生产 USSD 请求 wire 编码迁入 Rust；Iteration 410 将生产呼叫 INVITE/ACK/CANCEL/BYE/PRACK wire 编码迁入 Rust。Rust 负责有界字段校验、顺序序列化、Content-Length 和二进制 body；Go 仍负责请求事实、dialog/事务、SIP socket、响应、通话及 IMS 生命周期。Rust/Go parity、全量离线迁移门禁及365-scope契约 diff 通过；没有实机/运营商验收。3.08/build308 DMG 止于409。


## Iteration 413：生产拨号目标判断移除 Go fallback

现有认证 IMS codec operation 承担拨号目标校验、规范化、紧急号码禁拨和 USSD 短码拒绝；Go preflight 在状态/媒体变更前调用 Rust，并复用返回 URI 构造首个 INVITE。删除 Go 的重复生产规则与兼容 fallback，Rust 失效时失败关闭。Go 仍拥有 SIP/dialog/session、媒体和 socket 生命周期。全量迁移门禁通过：Rust workspace 1,421 passed / 3 ignored，Go 全包 test/vet、Swift BLE、2,722 carrier cases 及协议 parity 通过；契约 346 条（Rust286/Go60，49 HEAD aliases）、127 Swift HTTP calls、370 review scopes，44 项工具自测及 `--diff` 通过。Rust-only exit 仍有7类阻塞，无实机呼叫验收。P3 +0.025 至2.815/8.0，项目17.520/47.5=36.88%。最新 DMG 止于 Iteration412。详见[Iteration 413](../../iteration-413-progress.md)和[门禁日志](../../../build/optimization/iteration-413-migration-gates.log)。


### Iteration 420：Rust 批量构造 DTMF RTP 数据报

Rust 收到一次按键事实及当前 RTP 状态后构造完整 RFC 4733 数据报批次，Go 校验后在现有 UDP socket 上按 20 ms 节奏发送；媒体热路径仍未逐帧调用 IPC。全量迁移门禁及契约 diff 通过；P5 为 0.275/8.0，全项目 17.695/47.5（37.25%）。Go 呼叫/媒体 owner 和7类全Rust退出依赖仍未消除。


Iteration 429: 普通 IMS 入站短信必须有 Rust store_state=archived 确认才发送 RP-ACK，N待 Profile 事件 observer 保留。OPTIONS 请求构造、响应观察和心跳状态决策由 Rust 接口提供，Go 保留 socket、计时器与生命周期执行。此次 Go 入站归档回退在正常生产路径已冗余，进度不计分；详见[Iteration 429](../../iteration-429-progress.md)。


## Iteration 434 审阅范围补充

`POST /api/call-mode/enable` 现由 Rust 接收并校验明确确认字段，限制请求体并通过固定的内部认证 Go 适配器进入既有流程。未确认时 Rust 在任何 Go RPC/模块操作前返回原有 400 错误；确认后 Go 仍复核通话状态、持有 call-mode/radio/USB 操作锁、备份配置、写入模块 IMS/VoLTE 与 USB 配置并负责复位、重连和恢复。Rust 合成 UDS 测试验证确认路径和拒绝路径，Go adapter 测试验证认证和严格 DTO。完整离线门禁通过；这只迁移公开准入，不代表通话模式硬件流程已 Rust-only。

### 2026-10-13：Iteration 436 恢复备份设备绑定策略迁入 Rust

Rust 备份 owner 新增精确、认证的 `POST /api/internal/call-mode/backups/validate-restore`：它从 Rust 权威存储重读并验证备份，比较备份 IMEI/USB VID/PID 与当前设备事实，并保留现有不匹配错误文本。Go 删除本地绑定策略，采集实时事实并调用 Rust；写入前的呼叫/模块复核、操作 reservation、配置写入与重启仍保留在 Go 的硬件信任边界。Rust 测试覆盖匹配、不同 IMEI、不同 VID、不同 PID；Go 适配器测试覆盖认证头、请求 DTO 与 Rust 拒绝时 fail-closed。完整迁移门禁通过：Rust workspace 2068 passed/0 failed/6 ignored，Go test/vet、Swift BLE、2722 carrier replay 与 IMS/USSD parity 均通过；契约 diff 通过，门禁 SHA-256 `1cb8916a29a25013cdcb6a04079daca82e32b0d591f935c48117376e4129d7d8`。API census 354（Rust293/Go61）、127 Swift调用点、378审阅范围；Go退出审计仍有6类、266个生产Go文件。P2B +0.025，累计18.045/47.5=37.99%。没有实机、卡片或运营商验收；3.14/build314 仍冻结于Iteration435，不重打DMG。详见[Iteration 436](../iteration-436-progress.md)及[门禁日志](../../../build/reports/iteration436-migration-gates.log)。


### Iteration 437：同步上游 carrier selector display metadata

将 `ios-carrier-profiles` 更新至 `e5604370f93b863ee212dd3cdc5294979f11bca4`：profiles仍为683、selectors仍为853，仅10个selector YAML新增 `carrier_name` 展示字段。Rust profile owner与Go兼容resolver现在保留命中选择器的显示名；不将它用于PLMN/GID/ICCID/SPN匹配，也不改任何Ike/ePDG/IMS字段。Clarivate_IOS本地数据仍为 `d46fc4e`，其新增B0/B1/reader生命周期改动是iOS硬件层，未发现新carrier配置可移植。全量1,536份carrier profile/selector SHA lock、Rust `proletarius-vowifi` 测试、Go carrier tests及迁移门禁通过；门禁SHA-256 `745eafe91e7658dcedd8d625cdcce03d0ec5e58afabb69285963aa97cd0407b6`。API census保持354（Rust293/Go61）、127 Swift调用点、378审阅范围；Go退出阻塞仍6类/266个生产Go文件。迁移估值保持37.99%（18.045/47.5），本轮展示元数据不单独计分。没有设备或运营商实机验收；DMG仍是3.14/build314（Iteration435），未重打包。详见[Iteration 437](../iteration-437-progress.md)、[carrier审计](2026-10-12-carrier-refresh-audit.md)、[门禁日志](../../../build/reports/iteration437-migration-gates.log)。


## Iteration 439：N 待协作式定时任务关闭

N 待维护和 retry/registration-due scheduler 改用 watch 关闭通知；空闲时可立即退出，已提交的认证 Go UDS 通知则等待其原有 2 秒边界返回后才退出。daemon 在关闭 callback/internal listener task 集合前先 signal 并 join 两个 scheduler，避免 Rust 直接 abort 时丢弃已提交请求的等待方。只加强 P1 生命周期收口；Go 仍执行 profile/session work，未迁移新的业务 owner或增加迁移分。完整 `scripts/test-migration-gates.sh` 通过：Rust daemon 1447 passed/3 ignored、Go tests/vet、Swift BLE 回归、2722 carrier replay及现有 USB/radio/HTTP/IMS/codec parity。Cargo check/fmt、契约 `--diff` 通过；census 354（Rust293/Go61）、127 Swift调用点、378 review scopes。Go-only退出审计仍为6类阻塞/266个生产Go源。未连接设备或运营商网络，未生成DMG。3.15/build315 DMG 已由用户在此前要求生成，但其源码截止于Iteration438，不包含本轮修改。详见[Iteration439](../../iteration-439-progress.md)。

## Iteration 451：3 HK EAP-direct 进入初始 REGISTER 编码

Rust profile parser 现在读取用户模板 `initial_authorization: eap_direct`，并避免 `sec_agree=off` 把显式 EAP-direct 降成空 AKA 头。Initial REGISTER 编译器从同一 IKE/EAP-AKA 代次收到的零化 RES/RAND/AUTN 构造 Authorization；凭据缺失/无效时发送前失败关闭。Native tunnel owner 通过一次性凭据传递链将结果交给 generation-bound REGISTER builder；旧 registration API 和其他 carrier 行为保留。定向测试验证精确 builder output 与缺失凭据拒绝。

完整迁移门禁通过：Rust workspace **2,084 passed / 0 failed / 6 ignored**，Go 全仓 test/vet，Swift BLE 操作证明和虚拟管理器回归，以及 carrier、USB、radio、HTTP/HEAD、IMS/USSD/语音/SMS parity 全部通过。`cargo check -p proletariusd` 通过（17 dormant-code warnings）；契约 354（Rust293/Go61）、127 Swift调用点、378 review scopes，44 项扫描器自测和 `--diff` 通过。门禁日志 SHA-256 `267e1adb2f1b0a86e734d80aa6e82a91d0f5eaab23c7015d92fc67c5b94fccc8`。当前源码 Release `Switch.app` 3.15/build315 arm64 构建与深度严格签名验证通过；Release build log SHA-256 `0904903141b4329d1e988a5f20dfe3257d79fd2e05d2e3e8dbc120db78d5ba69`。

该切片仍没有真实 REGISTER/运营商验收，native lifecycle/protected IMS/live SMS-call gates 均关闭；不计 P3 端到端完成分，项目加权值保持 **18.045/47.5 = 37.99%**。Go build/run/embedded dependencies仍存在。没有实卡、短信或电话操作。上一轮生成的3.15/build315 dated DMG不含Iteration451；本轮不生成中间包。详见[Iteration 451](../../iteration-451-progress.md)。

## Iteration 459：全量离线门禁与 3.15 DMG 核验

本轮未改动生产代码或业务 owner。重新执行 `scripts/test-migration-gates.sh`：Rust workspace 1,460 passed/3 ignored，Go 全仓 test/vet、Swift BLE ledger/manager 回归（38组）、2,722 项 carrier replay 和 USB/radio/HTTP/IMS/USSD/call parity 通过。`check-rust-backend-exit.py` 显示6类 Go 退出阻塞，其中266个生产 Go 文件。按用户明确要求从当前源码构建3.15/build315 DMG，代码签名与 `hdiutil verify` 通过。未操作实体卡、短信、通话或运营商网络。本轮不计迁移点数，项目加权进度保持18.045/47.5=37.99%。见[Iteration 459](../iteration-459-progress.md)。

## Iteration 460：读卡 lease 内物理事务有界串行化

在 Rust `NativeReaderSessionLease` 的 PC/SC、BLE AKA 和 eSIM 卡操作 worker 共用 coordinator 事务 admission：FIFO mutex 保证单 lease 串行，32 个 semaphore slot 限制活动与排队任务合计；容量溢出复用 `ReaderTaskError::Busy`。排队后才登记 worker，并重新检查有效 reader generation/lease；容量与串行 permit 一并转移至真实 blocking worker，等待者取消不会提前放行正在执行的 APDU，lease drain 或过期后不会启动排队工作。定向测试覆盖串行顺序、32 项上限/permit 归还、drain 后拒绝。

定向 lease coordinator 测试 21 passed。最终源码完整 `scripts/test-migration-gates.sh`：Rust workspace 1,463 passed/3 ignored；Go 全仓 test/vet、Swift BLE completion/manager、2,722 carrier replay、USB/radio/HTTP method+HEAD 与 IMS/USSD/语音/SMS parity 通过。合同 census 354（Rust293/Go61，51 HEAD aliases）、127 Swift HTTP 调用、379 review scopes；合同 `--diff` 与 `git diff --check` 通过。审阅指纹明确记录本次调度边界，清单没有 API 变化。退出审计仍为6类 Go build/runtime/source 阻塞、266个生产 Go 文件。日志见[完整门禁](../../build/reports/iteration-460-migration-gates.log)、[合同差异](../../build/reports/iteration-460-contract-diff.log)和[退出审计](../../build/reports/iteration-460-go-exit-audit.log)。

本轮建立同一 lease 内读卡器事务队列，不解决每个 Profile 各自需要不同身份而 Gate 仅允许单一 reader pin 的架构冲突；Go `multiStandbyManager` 和真实 session/恢复仍未接管，native IMS gate 仍关闭。离线队列测试不能证明实体卡切换或 VoWiFi 运营商行为。本轮不增加端到端加权估值，整体保持 **18.045/47.5 = 37.99%**；无实机、卡片、短信、通话或网络操作。用户此前要求的 3.15/build315 iteration460 DMG 是队列代码变更前构建的，不含本轮实现，因此本轮没有再生成 DMG。详见[Iteration 460](../iteration-460-progress.md)。

## Iteration 461：N 待 Profile/Card 身份键归 Rust

N 待实体目录的 ProfileID/CardID 不再由 Go 从实体枚举事实推导。Go 向已认证的 Rust catalog owner 发送 reader、EID、ICCID 及展示元数据；Rust保留原哈希算法，负责输入约束、重复身份拒绝、稳定 ID 生成、目录事务及 mapping response。Go 验证响应逐一覆盖 EID/ICCID 后才能发布 Profile runtime/recovery map；当前物理 ICCID 的 ProfileID 仅从这份 Rust-owned ID map 得出。内部 catalog HTTP 写入拒绝调用方提供 ID 的旧格式，旧 Store fixture/migration 数据格式与原有 durable IDs 保持兼容。

定向 Go broker测试和 Rust store/私有 HTTP 测试通过。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1466 passed/3 ignored**，Go 全仓 test/vet、Swift BLE manager 38组、2,722 carrier replay、USB/radio/HTTP method+HEAD、IMS/USSD/call/SMS parity 通过。迁移合同 `--diff` 通过：354 contracts（Rust293/Go61、51 HEAD aliases）、127 Swift HTTP calls、379 review scopes；`git diff --check` 通过。门禁 SHA-256 `495347bbf7d3b0ce60008c6d06d254b242ed42a7adc29fc58e64878696e1e9f5`，日志[Iteration 461 migration gates](../../build/reports/iteration-461-migration-gates.log)。结构退出审计仍为6类、266个生产 Go 文件，详见[退出审计](../../build/reports/iteration-461-go-exit-audit.log)。该切片未接管 eUICC切换/复位、通知同步、AKA APDU/session 或 Go 多待生命周期，不计端到端加权进度；仍为18.045/47.5=37.99%。实机、短信、通话、运营商及长稳验收未进行；6982仍排除。之前用户要求的 Iteration460 DMG 源码截止在本轮前，本轮不重打包。详见[Iteration 461](../iteration-461-progress.md)。

## Iteration 462：更新运营商画像快照

载入 `iniwex5/ios-carrier-profiles` 最新检查版本 `cdba73e551810681f16b4cce0693c70e9e216c37`，Go embed 与 Rust JSON snapshot 同步为685 profiles/855 selectors，锁定1,544个来源文件 SHA-256。Clarivate_IOS 本地仍为 d46fc4e；新增 Lebara UK 与 DITO Philippines，Apple APN metadata 和 selector 国家代码均保留。旧 853 个 selector 的匹配标准/目标无变化，旧 683 个 profile 的 IMS 配置树无变化；既有用户运营商的 VoWiFi 参数未被改动。修订 DITO 515/66 基线、增加两张新卡的 Go/Rust resolver 测试，并把 replay 完整度校验改为依据当前 selector 导出用例数。

完整迁移门禁通过：Rust workspace 1,466 passed/3 ignored、Go test/vet、Swift BLE 38组、2,726 carrier replay、HTTP/USB/radio/IMS parity 通过。合同 `--diff` 通过：354 contracts（Rust293/Go61）、127 Swift HTTP 调用、379 review scopes。门禁日志 SHA-256 `11d03e6a91803ce9c7b2f974b827d845a6a6faf41f3884b06ff5a5bf9691817d`；退出审计仍为6类/266个生产 Go 文件。该数据更新不等于运营商实网支持：MODP group 2 与 peer certificate validation 尚未落地，APN provision 数据也不被 macOS VoWiFi runtime 消费。加权进度保持18.045/47.5=37.99%；无实卡、网络、SMS或通话验证，无 DMG。详见[Iteration 462](../iteration-462-progress.md)与[carrier审计](2026-10-13-carrier-refresh-audit.md)。

## Iteration 463：DITO 的 MODP group 2 / HMAC-SHA1-96 协议链

将 carrier 明确配置的 `AES128-CBC / HMAC-SHA1-96 / PRF-SHA1 / MODP group 2` 从 Rust 提案解析贯通到 SA_INIT、IKE SK 和 ESP CBC。启动时动态生成 carrier 指定的 DH KE，拒绝一组 IKE proposals 中混用不同 DH group；不会因强组失败而自动降级。DITO native proposal 转换、MODP-1024 公私钥、离线 group2 SA_INIT、IKE SHA1-96 SK/ESP 往返与篡改拒绝测试通过。MODP group 2 为 RFC 2409 的 1024-bit 旧算法，仅限显式声明该算法的模板。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace 2,104 passed/0 failed/6 ignored，Go test/vet、Swift BLE 38组、2,726 carrier replay 与 USB/radio/HTTP/IMS/USSD/call/SMS parity 通过。运行 workspace 全测时一个并发队列用例曾偶发 `Busy`，单项复跑通过，完整门禁复跑也通过。`git diff --check` 通过。门禁日志 SHA-256 `a842d08b188a231506d06d14d47d152a96e45690efa165acd6032cb13dc10193`；结构退出审计仍为6类、266个生产 Go 文件。协议覆盖增强未迁移新生产业务 owner，因此估值仍为 **18.045/47.5=37.99%**。未做实机或运营商验收，9eSIM `6982` 仍排除；未重建 DMG。详见[Iteration 463](../iteration-463-progress.md)。

## Iteration 464：DITO SHA1-96 CHILD SA 数据面闭环

DITO 本地 ePDG/SOCKS5 UDP loopback 首次触发 child data-plane 拒绝：`PendingChildSa::into_data_plane` 虽接收 proposal transform 2 并可构造 SHA1-96 ESP 收发器，却只允许 SHA2 transform 安装数据面。现已将 HMAC-SHA1-96 纳入已声明 CBC integrity transform 匹配，并扩展 startup loopback 按实际 DH group 选择模拟响应端密钥。完整离线用例覆盖精确 DITO proposal/KE、group 2 SA_INIT、EAP-AKA、IKE_AUTH/CHILD SA、双向 ESP SIP、DPD、损坏包和 peer DELETE；应用错误用例确认不会误留数据面。

并行 workspace 全测曾再次复现 physical-card queue `Busy`：物理 worker 标记与 FIFO permit 的析构次序允许后继 worker 在旧标记清除前获得锁。Rust lease worker tuple 已调整为先释放物理 worker 标记、再释放 serial permit。定向 queue 测试及完整并行 workspace 测试随后通过。

验证：`cargo fmt --check`；IKE **43 passed**；VoWiFi **190 passed**；全量迁移门禁 **1,466 passed / 0 failed / 3 ignored**，并包括 Go test/vet、Swift BLE、carrier replay、USB/radio/HTTP/IMS parity；迁移合同 `--diff` 通过（354 contracts：Rust293/Go61，127 Swift HTTP calls，379 review scopes）；`git diff --check` 通过。协议测试 SHA-256 `ed93beb0652e7e46bf42a6ceb9d2f1e12d5eee3182cee4c0943583f9238e2000`；门禁 SHA-256 `1e083b17c5122a193ff8242d1667dad34d99e98a83dd8cfc6fd7ba2e3f65b70a`；见[Iteration 464](../iteration-464-progress.md)、[门禁日志](../../build/reports/iteration-464-migration-gates.log)和[合同差异](../../build/reports/iteration-464-contract-diff.log)。

这是合成离线测试，没有真实 SIM/ePDG/IMS 证据，也没有切换生产 VoWiFi owner；项目进度仍 **37.99%**，Go 退出审计仍6类/266个生产Go文件。9eSIM `6982` 不在本轮范围；本轮未生成 DMG。

## Iteration 470：通话音频控制与语音切换执行移交 Rust

Rust 接管 `POST /api/call/audio/start` 与 `POST /api/call/audio/stop`，并复用共享 ADB/USB voice-transition executor；Go 只保留认证的 readiness 事实适配器。完整离线迁移门禁通过：Rust workspace 1,476 passed/0 failed/3 ignored，Go 全仓 test/vet、Swift BLE、2,726 carrier replay 与 USB/radio/HTTP method+HEAD/IMS/SDP/AMR/MESSAGE/USSD/dial-target/rejection parity 通过。合同 census 355（Rust295/Go60，51 HEAD aliases）、127 Swift调用点、382 review scopes；contract diff、Rust fmt、git diff check 通过。门禁 SHA-256 `84817b14880215dbd4956a3c1e3e7fd904eafafa77bf5ba533c10074aea54eb7`。退出审计仍有6类Go阻塞、266个生产Go源文件。该切片计P2B +0.025，累计18.070/47.5=38.04%（±3个百分点）。离线测试不等于实机通话验收；未访问读卡器/号卡/运营商网络，也未生成DMG。Clarivate_IOS只读复核和carrier regression见[Iteration 470](../../iteration-470-progress.md)。

## Iteration 471：音频启动与通话模式变更的跨进程互斥

Rust 音频启动先获取 `call-mode-operation.lock` 共享锁，再读取 Go readiness 事实；Go 通话模式硬件变更和 Rust 备份删除继续用同一文件的独占锁。shared guard 随 detached ADB worker 延续，直到 USB voice-transition lease 清理结束，取消 HTTP 请求不会提前释放。Go readiness adapter 不再读取 in-memory `callModeOperation`，但 dial/answer 仍保留其旧 admission 检查；Go 仍持有实际 call-mode mutations 和 readiness facts。双向锁互斥测试、Rust daemon 1,477 项通过/3 忽略、Go package tests、全量 migration gates、contract census diff 与 `git diff --check` 通过。门禁 SHA-256 `ed2e02db858ce7eb7a6b0f6fbd267b1c9032c654bcca89d9cfa114ced7f6781c`；census 保持 355 method/path、Rust295/Go60、127 Swift calls、382 review scopes。未迁移完整 call-mode owner，无实机验收，因此不增加加权进度，仍为 38.04%。上一轮要求生成的 Iteration470 DMG 不含本轮源代码；本轮未再生成 DMG。详见[Iteration 471](../iteration-471-progress.md)。


## Iteration 472：通话音频 readiness 的硬件事实直接迁入 Rust

Rust 通话音频启动现在在共享通话模式文件锁与 module-radio reservation 下直接读取 USB composition/IMS/VoLTE，调用 Rust ADB 探测、运行时文件校验及模块部署代次检查。原内部 Go readiness 路由和适配器已删除。USB ADB/UAC 未准备好时先返回，不额外查询 IMS/VoLTE，保留旧操作顺序。通话模式启停、部署及恢复仍由 Go 管理，因此这只完成音频启动 readiness 子流程。

完整离线门禁通过：Rust workspace **1,480 passed / 0 failed / 3 ignored**，Go 全仓 test/vet、Swift BLE、2,726 carrier replay、USB/radio/HTTP method+HEAD/IMS/SDP/AMR/MESSAGE/USSD/dial-target/rejection parity 通过。合同 `--diff`、Rustfmt、`git diff --check` 通过；census 354（Rust295/Go59、51 HEAD aliases）、127 Swift调用点、382 review scopes。门禁 SHA-256 `981ae59b3c9c12787d312cff530bedc931993e07f9e4b50cae5abee49b300c17`；合同差异 SHA-256 `f48a1047183edd150b906840139160b0a1cf8d82103e61fec6807eb8a1bcd438`。结构退出审计仍6类/266生产Go源，见[审计日志](../../build/reports/iteration-472-go-exit-audit.log)。

本轮将该 P5 readiness 子能力计入 +0.025 人周，累计 **18.095/47.5=38.09%（约±3个百分点）**。未做实机通话、读卡器或运营商验收；6982继续排除。用户要求构建的 [3.15/build315 DMG](../../dist/Switch-3.15-macOS27-iteration472-20261013.dmg) 通过 hdiutil、包内签名、版本和 arm64 核验，SHA-256 `4caaee840d10b5441adb1fcb0949fc808032ddb1907b6b25b9d37e6ebeb2900e`；未公证。详见[Iteration 472](../iteration-472-progress.md)。


## Iteration 473：移除 Go AT-console 兼容执行残留

`POST /api/at` 的正常 supervised 请求已由 Rust gateway 转发至 Rust module-radio owner，Go 的公开 handler 只保留在显式兼容启动路径。本轮删除该 Go mux registration/handler、重复 Go parser/result wrapper、无生产调用者的 `nativeRadioDiagnostic` client 与相应遗留测试。保留 Rust 对命令语法、飞行模式蜂窝命令拦截、VoWiFi AT 通道排他、结果分类和未知执行结果不自动重放的策略。合同库存现仅列 Rust tuple dispatch，Go dependency 为空；路由层状态仍不证明整个 daemon 已摆脱 Go 子进程。

定向 Rust `public_at_console` 测试 **5 passed**；Go package test 通过；完整迁移门禁通过：Rust workspace **1,480 passed / 0 failed / 3 ignored**，Go test/vet、Swift BLE、2,726 carrier replay 及 USB/radio/HTTP/HEAD/IMS/SDP/AMR/MESSAGE/USSD parity 均通过。合同 `--diff`、Rustfmt 和 `git diff --check` 通过。census 为 354（Rust295/Go59、51 HEAD aliases）、127 Swift调用点、381 fingerprinted review scopes。门禁 SHA-256 `abad3d9175e05132ad15c7bf0d685e37da640a80b5765fd0b7e64a4112c9ece6`。Go 退出审计仍有6类阻塞，生产 Go 文件由266降至265；详见[审计](../../build/reports/iteration-473-go-exit-audit.log)。

本轮只移除已经有 Rust 生产实现的兼容残留，不重复增加功能迁移估值；项目保持 **18.095/47.5=38.09%（约±3个百分点）**。没有操作实体卡、短信、电话或运营商网络；9eSIM `6982` 继续排除。Iteration473 Release DMG 已按用户要求生成并通过镜像与包内签名检查；完整构建日志见 `build/reports/iteration-473-release-build.log`，镜像校验日志见 `build/reports/iteration-473-dmg.log`。

## Iteration 474：运行时下载进度由 Rust 持有

运行时下载进度先前由 Rust worker生成，再逐次回调Go，导致Go副本成为界面状态的最终来源。现在Rust `JOB`快照是进行中 byte/detail 的来源；call-mode status只在Go活动快照仍为downloading且source/runtime version/total完全匹配时叠加进度。终态complete/failed仍回调Go，以沿用现有模块部署与失败生命周期。Go拒绝中间态进度写入，因此不再镜像Rust下载流。

离线验收：Rust下载器14项、call-mode status代理1项、Go package tests和完整迁移门禁通过。Rust workspace **1,481 passed / 0 failed / 3 ignored**，Go全仓test/vet、Swift BLE、carrier replay、USB/radio/HTTP/HEAD/IMS/SDP/AMR/MESSAGE/USSD parity通过；Rustfmt、合同`--diff`、`git diff --check`通过。合同 census 354（Rust295/Go59，含51 HEAD aliases）、127 Swift调用点、381 review scopes。门禁 SHA-256 `e65b2fed495e63a7cf3dfd4a15e643df0f98484de142240ec40e39ce584b0af9`，合同差异 SHA-256 `fb195042cf70ee9a037e9f7defac9ea343ee1cd75e16505ee30b228924494c49`。Go退出审计仍6类阻塞、265生产Go文件。

按P2B +0.025计划人周等值，P2B **4.950/8.0（61.88%）**，整体 **18.120/47.5=38.15%（约±3个百分点）**。这不代表下载全生命周期完成；Go仍持有硬件准入、operation reservation/watchdog及部署/恢复。无实卡或运营商验收，9eSIM `6982`仍排除。用户随后明确要求并已生成 [Iteration474 DMG](../../dist/Switch-3.15-macOS27-iteration474-20261013.dmg)，SHA-256 `7de616190e794d4ff81c52e5dacf783fcbdfb55fa6bb9f2109262639f08361f8`；该包不含Iteration475。详见[Iteration474](../iteration-474-progress.md)。


## Iteration 475：通话模式重启拒绝判定归 Rust radio owner

Rust radio plan 将明确的 ERROR/CME/CMS 应答行编码为 `radio_reboot_rejected`；Go 启用和还原流程只消费类型化错误，不再重复解析 AT 文本。已知拒绝保留原错误提示，超时/响应不确定继续进入现有重连核验，避免把可能已执行的 CFUN 重启误判为未发生。Rust radio 专项、Go call-mode 定向测试及完整离线迁移门禁通过：Rust 1,482 passed/3 ignored，Go全仓 test/vet，Swift BLE、2,726 carrier replay、USB/radio/HTTP/IMS/codec parity 通过。契约为354（Rust295/Go59）、127 Swift调用点、381 review scopes；`--diff` 和 fmt 通过。P2B +0.025 至4.975/8.0，整体18.145/47.5=38.20%。6类 Go 退出阻塞和265个生产 Go 文件仍在；通话模式 reservation、watchdog、模块部署/恢复仍属 Go。没有实机验收；Iteration474 DMG早于本轮变更，不含本轮代码，未重打包。门禁SHA-256 `923f6b0172bbc04b6b714af9f76b8e420c731dbead12f71c647997d4585756dd`；Go退出审计SHA-256 `7f4cca16498da7a2e6483ffd5bf8843cfb6a02bec09549fa6ed1914efd00aa16`。详见[Iteration 475](../iteration-475-progress.md)。
