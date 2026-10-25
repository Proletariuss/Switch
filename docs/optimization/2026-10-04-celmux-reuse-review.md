# Celmux Rust 源码复用评估

初始评估：2026-10-04；按用户提供的 `/Users/proletarius/Downloads/celmux-src/` 多次复核并择项复用。参考目录始终只读，没有修改。快照不含 `.git` 元数据，不能核验 Git revision；复用来源以文件级许可证和 SHA-256 固定审阅对象。

## 结论

Celmux 与 Switch 的技术相似度高，其可独立复用的 MIT 协议层已被用于 Switch 的 CCID/eUICC/LPA、QMI wire/client/services 与 SIP core/transaction runtime。AT session 的响应组装、提示符交互与单 reader 边界作为行为基准适配进 Switch actor；没有复制 Celmux 的 AT 应用层或第二套设备 owner。

当前 USB/PCSC 下载入口已经由 `proletariusd` 执行：Rust 服务调用复用的 SGP.22 LPA、Switch 自有 `HttpEs9Client`、PC/SC APDU adapter，并维护确认、取消、durable operation journal 和异常后“结果待核对”状态；SM-DP+估计大小及确认流程已在产品接口中。该接管仍是分段迁移：Rust 会通过 Go 兼容接口关闭读卡器 VoWiFi、获取并续期 generation-bound reader lease、释放租约后刷新旧读卡器缓存；BLE 下载仍走 Go，且尚无实体卡验收。9eSIM `6982` 与鉴权绕过继续排除。

## 复用边界与优先级

| Celmux 源码 | 与 Switch 的缺口匹配 | 处理决定 | 主要差异 / 门禁 |
|---|---|---|---|
| `crates/at/src/session.rs` | 一端口一个 worker、FIFO 命令队列、URC 分流、终态识别、超时、`>` 交互、响应上限；约1,074行并有内存串口回归测试。Switch 已在 Rust actor 独占串口的基础上接入有界事务和SIM/+CMTI事件解析。 | **事务边界、提示符收集和单读者设计已参考并适配；RDY/CPIN/QSIMSTAT和+CMTI已变为typed frame metadata。** Switch 使用自己的 bounded actor 与返回类型，没有复制整套 Celmux crate；Go 暂时仍负责状态回调及短信读取副作用。 | Switch 的高优先级短信/网络命令仍须保留。`AT+CMGS` 命令与 PDU/Ctrl-Z 必须作为不可插队的同一事务；SIM epoch 改变要在提示符后阻止载荷提交；URC 要维持旧异步/响应型分类及状态副作用；保留 APDU/storage 锁和 watchdog。Celmux 的无界事件 channel、超时丢弃输入行为也需改为 Switch 的有界与可审计策略。该 crate 为 PolyForm Noncommercial，不从中复制源文件。 |
| `crates/at/src/port.rs`、`port/unix.rs` | macOS POSIX 串口实现、poll/termios、输入清空与端口 lease；Switch 已有串口由 Rust actor 打开并所有。 | **只复用平台行为和错误处理，不新增第二串口 owner。** | Celmux 使用 `nix 0.31.3`；Switch 锁定 `nix 0.30`。同一路径只能有一个进程级 owner。串口 flush、关闭还原、拔出和端口 lease 必须在 Switch 现有 actor 内整合，禁止 Celmux Session 另行 open 相同 `/dev/cu.*`。 |
| `crates/at/src/modem.rs` | 有 Qualcomm/Quectel typed identity、SIM、注册、信号、短信、USSD、通话和 eUICC APDU command 封装。 | **按命令/解析函数择优迁移，暂不复制整套 `Modem`。** Switch 的 `QueryOperator` 现通过 Rust typed-query 执行固定 COPS 格式选择/读取和 PLMN 字段解析；只参考 Celmux 的字段行为，没有复制 PolyForm AT crate。 | Switch 的短信编码与 durable ack、SIM epoch、QMI 旁路、eUICC/读卡器仲裁及回调语义不同；照搬公共 API 会绕开现存业务不变量。COPS 输出格式设置失败仍继续读，与旧 Manager 一致。 |
| `crates/sms/src/lib.rs`、`crates/gsm/src/lib.rs` | MIT许可、较小的 GSM 03.38 与 SMS TPDU/RP codec，可用作互操作向量和边缘行为的对照基线。Switch 已把 SMS crate 拆成 address/alphabet/UDH/reassembly/status-report/OMA-CP 等独立模块，覆盖面更广。 | **对照测试与差分样本复用，不替换现有实现。** | 当前 Celmux SMS codec 聚合在单个 `lib.rs`，依赖它的 `celmux-gsm` 同为本地workspace包；把 crate 直接接入会重复编码器并给项目新增无必要的来源/依赖边界。任何源码摘取须保留MIT版权头与许可。 |
| `crates/sip/core/{lib,dialog,transaction}.rs` | 独立 SIP message/framing、dialog/CSeq/route-set 与 RFC 3261/6026 事务状态机，不依赖网络 runtime；Switch 的 Rust `proletarius-ims` 当前集中在 REGISTER/AKA/sec-agree，完整 call dialog 与事务 owner 仍在 Go。 | **已接入独立 `proletarius-sip` crate**，复用纯协议类型作为后续 IMS/SMS/语音迁移基础；保留单独 parser、reader、Go owner 和 Swift API，不做生产切换。 | 三个已接入源文件均为 MIT 并保留文件头；manifest 虽写 BSD-2-Clause，不据此扩大源代码授权。`auth.rs` 是 BSD-2-Clause，未复制：Switch 已有 AKA/Digest，当前不需要重复实现。还要用 Go 脱敏轨迹差分 ACK/CANCEL、PRACK、重传和来电/挂断竞争，之后才接 production dispatcher。 |
| `crates/ims/src/{registration,sms,call_session}.rs`、`crates/vowifi/session/src/{managed,liveness}.rs` | 有在线 IMS/短信/通话状态机、注册和 liveness/recovery 样例；直接针对服务生命周期。 | **作行为对照和故障矩阵参考；不整包复制。** | Celmux 的 carrier policy、transport、smoltcp、media 与会话所有权紧密耦合。Switch 已有大量 Rust AKA/IKE/ESP、VoWiFi supervisor 和 SIP REGISTER 行为；应将差异沉淀到 Switch 的代次、掉线、renew、旧会话退出测试中。 |
| `crates/euicc/ccid`、`crates/euicc/euicc` | 传输无关 APDU、BER-TLV、logical channel、ES10c profile 管理、通知与 BPP 分段；与 Switch 正常读卡器 eSIM 能力重叠。文件级 MIT。 | **已接入 `proletarius-ccid` / `proletarius-euicc`，并由 Rust USB/PCSC 下载、Profile 改名/删除/启用、通知列表自动清理及手动通知重试路径使用。** 保留源版权/SPDX；PC/SC 传输和设备所有权使用 Switch 自有实现。 | 已有离线 fake APDU 与 Rust route 测试；实体卡差分验收仍缺。AID 顺序、CAT_BUSY 重试、notification refresh、Go 租约/VoWiFi 协调和 reader generation 必须继续保持 Switch 语义，不能照搬 Celmux 高层策略。 |
| `crates/euicc/lpa/src/{activation,lib}.rs` | MIT 传输无关 SGP.22 状态机：AuthenticateClient/AuthenticateServer、确认码哈希、用户确认、限时取消、BPP 安装和 notification retry。 | **已接入 `proletarius-lpa`，并用于 Rust USB/PCSC `/api/reader/esim/download` 生产路由；Rust operation journal、轮询、确认及取消服务调用同一状态机。** Switch 自有 `HttpEs9Client` 保留 GSMA CI 根证书、Admin Protocol 2.5.0、直连、禁重定向和有界响应体。 | Go 仍提供 VoWiFi 关闭、generation-bound reader lease acquire/renew/release 和最终身份缓存刷新；BLE仍由Go处理。mock/兼容测试通过但无实体卡验收；9eSIM `6982`不在范围内。 |
| `crates/celmux/app/src/downloads.rs`、`operations.rs`、`server/esim.rs` | 应用层 eSIM 任务生命周期：单活动任务、有界记录、operation ID、进度、确认、取消和轮询/SSE。 | **只采纳约束，不复制 app 源码。** Switch 自有 `reader_esim_download.rs` 已实现单活动下载、耐久 journal、用户确认、取消、lease loss、进程中断后的待核对状态和旧 API DTO；Rust 生产下载路由已接入。 | Celmux app 文件属 PolyForm Noncommercial；Switch 的实现还要桥接既有 Go owner，并按读卡器身份/代次分租约，不能照搬 DeviceId 生命周期或在写卡不确定时自动重试。 |
| `crates/qmi/wire` | MIT、`no_std`、无设备 I/O 的 QMUX/QRTR framing 与 QMI TLV codec；和 Switch Go QMI `Packet.Marshal` 的普通 QMUX 形状直接对应。 | **已接入 `proletarius-qmi-wire`；新增 Switch-owned QMUX/CTL golden bytes 与 QRTR round-trip 测试。** | wire codec 不是单独的 QMI 接管证明；Iteration 076 已将它与 Rust libusb、QMI client 和 typed services 接成 opt-in 生产 owner。QRTR仍只是兼容能力。来源 `lib.rs` SHA-256：`b7c7e61ccefa57fb34dc9e8d86841e96326babcff3e9db0f0188cf44eaff2cfc`。 |
| `crates/qmi/services/src/{ims,nas,uim}.rs` | MIT、IMS/IMSA service IDs/TLV maps、NAS Get Operator Name、UIM transparent/record/file-attribute reads 和按 service 分离的 client-ID 生命周期。与 Go 的 QMI settings、注册状态、运营商显示名及 home MCC/MNC 查询重合。 | **IMS/IMSA get/set/status、NAS operator name 与 UIM home MCC/MNC 查询 facade 均已适配；每项均有 fake transport 或 golden/parser tests。** | UIM 仅读 EF-OPL、IMSI、EF-AD，当前已接到 `switchd-rs` 私有 QMI owner；断链/失效句柄恢复、超时阈值与禁止写重放已有 fake session 验证，实体模块差分仍缺。Go Get IMS 不暴露 Celmux call mode；显式 false、TLV次序、EF-OPL→IMSI/EF-AD优先级按 Go 保持。 |
| `crates/qmi/core/{lib,fanout,qmap,transport/mod}.rs` | MIT QMI client 生命周期、并发事务匹配、CTL client-ID、响应超时与隔离 indication fan-out，和 Switch 的 macOS QMUX 客户端核心直接重合。 | **已纳入 `proletarius-qmi-core`，保留 caller-owned `Transport` 注入点；复用它来承接 Switch 专属 libusb transport。** | 保留源文件版权/SPDX，适配 wire crate 名称；省略 Celmux Linux proxy/Windows USB transport 和 Celmux logger。fake transport 三项覆盖 CTL 分配/释放、并发乱序响应和迟到超时响应。 |
| Go `transport_libusb_darwin.go` 的行为边界 | 当前实际可达的 macOS QMI USB 路径：VID/PID 优先级、ADB 排除、DJI ff/ff/ff 候选规则、interface claim、CTL_SYNC 和 bulk timeout。 | **Switch 自有 `proletarius-qmi-libusb` 已实现 Rust transport；只移植行为约束，没有复制 Go/C 源码或在测试中接触设备。** | `rusb` 0.9.4 封装 libusb；offline 测试覆盖 selector 与接口候选规则；该 transport 已接入 `switchd-rs` 的单一 QMI owner，代码实现 interface claim/alternate setting/CTL_SYNC/单 owner/有界 bulk read-write/shutdown。尚无实机 I/O 或断连恢复验收。 |
| `crates/qmi/{manager,qrtr}` 与其他 services | Celmux manager 包含逐 service rebind/indication replay、SIM/card gate、NAS、UIM/WMS/DMS、WDS data plane；目标系统更宽，也覆盖不同平台。 | **只采纳按 service 恢复、 indication 重新注册和单设备 owner 的设计模式；按 Go 可达 API 挑窄接口实现，不整包移植。** | Switch 当前 Go compatibility adapter 仍只在 `-backend=qmi` 路径启用，主模式仍为 AT；Rust 已独占 macOS libusb、IMS/IMSA/NAS/UIM clients，并与 Go QMI transport 互斥。后续补 Go manager 断连重连/rebind/replay 的可达语义和实体回调差分。Linux QRTR、`/dev/cdc-wdm*`、Windows USB、`qmi-proxy` 与不使用的数据面不带入。 |
| `crates/desktop/*`、Web、数据库、配置与日志 application 层 | 与 Switch 保留的 SwiftUI 和现有数据/配置文件边界不同。 | **不复用。** | 不引入第二套 UI、SQLite 或数据迁移；Switch 已明确保留 SwiftUI 且保持现有数据格式。 |

## 许可证与来源要求

许可按 crate 与源文件核对，不能从 Celmux root license 推断。AT session 仍是行为参考；本轮复用 manifest 和逐文件头均标为 MIT 的 `ccid`、`euicc`，并移植 `lpa` 的纯状态机/activation parser。Switch `THIRD_PARTY_NOTICES.md` 保留版权/SPDX并附完整MIT条款。Celmux LPA HTTP/DNS client、AT、QMI、IMS、VoWiFi、应用/UI并未复制。根 PolyForm 的 VoHive/Celmux Required Notice 仍只适用于其对应源代码范围。

后续移植仍必须保留文件级来源/ SPDX/版权头，在 Switch `THIRD_PARTY_NOTICES.md` 记录链接、许可和用途，并附适用的许可文本。不同 crate 的 MIT/BSD/PolyForm 边界不能由一个 workspace license 覆盖；SIP crate manifest/header 差异须先查清。

本次评估锁定的候选文件摘要：

| 路径（相对 `celmux-src`） | SHA-256 |
|---|---|
| `crates/at/src/session.rs` | `79a97e3863bcc90eadc3dd7bfba186477caa4a66343c22532b6201b5a74933e6` |
| `crates/at/src/modem.rs` | `fb48868cfd48870512b3f42fcac8d13f6d434df92e78cbef430e2cc381e666f2` |
| `crates/at/src/port.rs` | `86115ec907cc92705a99941a01af8c0817957fe8b611e5a53eb532cdde911fa5` |
| `crates/at/src/port/unix.rs` | `befdfaf27800f12fd52374e1848bd63cce602501daafc488fb3226a5f2ce31c7` |
| `crates/sip/core/src/lib.rs` | `e52b4d917da2b571079668f6b041fddd1747ea0d4ed2503c39ba8003da3be15d` |
| `crates/sip/core/src/auth.rs` | `24e5be1adecaa6fcf477df685f435c34ee187c47b1081e8bf96d8ad5ae4df52c` |
| `crates/sip/core/src/dialog.rs` | `ff8b68d2911e99d8b03fa1c00b8d79c6d4d576d3be4a5b9c517d5c9c37948b0b` |
| `crates/sip/core/src/transaction.rs` | `07fc76aa035cc0e8aa32dc27dd212d3567969514b5024024b10efcedddb67563` |
| `crates/vowifi/session/src/managed.rs` | `332be45e47858526e91da203056be1cb8d9cc4dcc1020543eefac4a623cbe0c9` |
| `crates/vowifi/session/src/liveness.rs` | `1fd357d7f99431929542a9e82064aad5319b178a678d32939cbb36276230616d` |
| `crates/euicc/lpa/src/lib.rs` | `e9e0ac7015ecaf5b9ce9538ccf547ac53a3a71785a5a58d03ec0aa9a8f350370` |
| `crates/euicc/lpa/src/activation.rs` | `5cf66df6bb12f325c0d63dd8d51ed5c500e09cec55610f89a0f2502187c95685` |
| `crates/euicc/lpa/src/es9.rs` | `4a1371d659ddf8a2235bd33c38bc80845501638a5f5f53720cff6dfc990ed877` |
| `crates/euicc/ccid/src/lib.rs` | `03b294d03ca62aa604ce606104720f7751a5a7a4468911452d3a3931b2dfca31` |

## 2026-10-05：Celmuх operation/设备锁边界复核

从 Celmux 的下载应用层进一步确认，最值得带入 Switch 的是“operation 状态由应用核心持有、协议状态机同步执行、设备 owner 由调用方提供”的分层。它的进度事件有界且带 cursor，终态短期保留；同步 eUICC/HTTP任务放专用 worker，确认回调等待 UI 响应。Switch 可以保留自身 operation DTO 和轮询 API，仅参考这些生命周期约束；Celmux 应用任务源文件的 PolyForm Noncommercial 许可不允许将其当作 MIT 协议 crate 复制。

已在 Switch 的 `proletarius-reader` 新增 `with_reader_operation`，让完整同步 APDU/PCSC 工作流共享既有 per-reader try-lock：同卡冲突立即作为 `SharingViolation` 返回，不跨 HTTP 请求排队；不同读卡器锁相互独立；闭包结束后锁自动释放。Rust reader crate **26 项测试通过**，含并发/释放行为；`cargo +1.98 fmt --all -- --check` 与 `git diff --check` 通过。这个 helper 还没有被生产 eSIM route 调用，也没有取得 Go 的跨进程 lease，因此整体进度不增加，仍为 **17.5%**。

## 2026-10-05：复用 Celmux 的传输无关 LPA 状态机

将 `crates/euicc/lpa/src/lib.rs` 与 `activation.rs`（保留 2026 anti-rainer copyright 与 MIT SPDX）移植到 Switch `rust/crates/proletarius-lpa`。接缝使用现有 `proletarius-euicc` 的 `RspCard`/`Euicc<T>` ES10b 操作、`sha2` 和 workspace `http::Uri`；从 `Es9Transport` trait 传入网络，不复制 `HttpEs9Client`、`ureq`、Celmux DNS 或关闭证书验证的实现。网络取消失败由 `DownloadObserver::on_warning` 暴露给宿主，同时保留主网络错误。测试使用纯内存 fake card/fake ES9，不连接服务器、读卡器或实体卡，覆盖正常下载顺序、用户拒绝取消、网络错误保真、notification 只在成功后移除以及 activation/confirmation code 校验。

验证：`cargo +1.98 test --offline -p proletarius-lpa -p proletarius-euicc` 的5项LPA状态机测试和3项eUICC既有profile codec测试通过；锁定工具链缺 Clippy component，因此 Clippy不可用，未安装组件。后续迭代已将 ES9 transport 与完整下载确认相关 metadata error 校验和 size estimate 加入隔离 crate 回归，详见 [Iteration 057](../iteration-057-progress.md) 与 [Iteration 058](../iteration-058-progress.md)。`proletarius-lpa`仍未被`proletariusd`或App生产下载路由调用，operation恢复账本、生产UI大小确认、reader lease和设备路径未迁移，所以本次不增加整体17.5%迁移进度；9eSIM `6982`仍明确排除。

## 后续执行顺序

1. **当前 P2B：硬件/短信责任收口与LPA生产接线。** AT事务、有界响应、URC分流、超时决策、代次观察、CPMS/CMGR/CMGL读取和RDY/CPIN/QSIMSTAT类型化解析已进入Rust actor；Go仍消费事件、确认短信耐久归档并触发既有回调/恢复。LPA协议状态机、ES9+传输及下载大小确认所需的profile元数据校验/参考估算均已离线验证；接下来接入现有下载路由、operation账本和唯一reader session lease。真实卡操作仍须等离线行为等价门禁通过后才执行。
2. 增加并通过 Switch 专属回归：高/普通优先级、`+CMTI` 与响应型 URC、`AT+CMGS` prompt+payload 原子性、超时后迟到响应/清理、watchdog、拔卡/RDY 的 epoch fence、APDU/storage 仲裁、关闭排空。完成前不提升硬件接管进度。
3. **P7 SIP：** 纯协议 parser/dialog/transaction primitives 已接入 `proletarius-sip`。下一步先把 Switch Go-SIP 脱敏轨迹转成 golden fixtures，差分 ACK/CANCEL、PRACK、事务重传与来电/挂断竞争；随后把通过的生产业务逐个交给 Rust IMS/SMS/voice owner。完整来去电和恢复故障矩阵仍是门禁。
4. **P2B LPA/QMI：** 等串口/APDU 唯一 owner 形成后，再按普通 GSMA 与实际 macOS 设备传输逐项采纳；协议包编译通过不代表硬件能力上线。

## 2026-10-05：对用户新提供的 `celmux-src` 快照复核

对 `/Users/proletarius/Downloads/celmux-src/` 重新核验后，10 个已接入 CCID/eUICC 文件的 SHA-256 与此前审阅表一致，说明本轮看到的协议源码与已复用版本相同；快照根目录没有 `.git`，因此无法确认提交号或其分支状态。依赖/授权以 crate manifest 和单文件 SPDX 头共同判定，不把参考目录内的 `AGENTS.md`、构建脚本要求或 PolyForm 项目级规则当作 Switch 的执行指令。

额外检查 `crates/euicc/ccid-host`：manifest 为 MIT，但 `lib.rs` 只在 Linux/Windows 编译 host transport，其余平台编译 `unsupported`；其 PC/SC IPC、热插拔 watcher 和原生 USB CCID backend 不能作为 macOS 27 读卡器 owner 直接复用。Switch 已有 `pcsc` crate、macOS PC/SC reader scanner/session 与共享读卡器操作锁，接入该 host crate会多出重复设备层而无 macOS 实现收益，因此记录为**不接入**。

本轮复用 `crates/sip/core` 的纯 SIP parser、dialog 与 RFC transaction primitives，把 `lib.rs`、`dialog.rs`、`transaction.rs` 接入 Switch 新增的独立 `proletarius-sip` crate：实现层没有引入网络 runtime，离线回归覆盖有界解析、分片/合并 stream、keepalive、CSeq/Route-Set、ACK/BYE、客户端重传/终态和服务端 CANCEL。`auth.rs` 按文件头为 BSD-2-Clause，未复制；Switch 有自有 AKA/Digest，当前不值得扩大许可文本和重复算法。新 crate 尚未接入 production SIP owner，也没有替换 `proletarius-ims` 的严格 REGISTER parser 或 Go IMS。下一步应先把 Go-SIP 脱敏轨迹转为 golden fixtures，再对 ACK/CANCEL、PRACK、来电/挂断竞争和事务超时差分；通过后才接 Rust dispatcher。不应引入 Celmux 整个 IMS/VoWiFi runtime：carrier、N待多 Profile、卡片/会话所有权与 Switch 目标不同，liveness 和 managed-session 的时序适合作为故障矩阵参考，不能直接覆盖 Switch owner。

SMS/GSM crate 与 Switch 已有更细分的短信编解码、状态报告、二进制与 OMA-CP 处理重复；保留 Celmux 编码轨迹作差分测试，不替换实现。QMI MIT wire/client core、Go-compatible IMS/IMSA/NAS/UIM codecs 与 macOS libusb transport 已接入 `switchd-rs` 生产 owner，Go 的 `-backend=qmi` 作为 compatibility adapter 调用 Rust 私有 UDS。mock route 投影已测；没有 Go QMI trace replay、断连恢复或设备验收。当前复用覆盖 CCID/eUICC/LPA、SIP transaction/dialog、QMI transaction/service client，以及 AT 单事务行为适配；仍不引入 Celmux IMS/VoWiFi 应用 runtime。

本次源快照复核没有新增 vendor 文件或扩大 license 范围，也没有修改参考文件夹；后端推进和验证记录见 [Iteration 063](../iteration-063-progress.md)。

## 2026-10-05：Celmux IMS/IMSA service-client 生命周期适配（Iteration 072）

此次检查 `/Users/proletarius/Downloads/celmux-src/` 的 QMI service wrapper 后，沿用其“每个 QMI service 自己申请/释放 client ID”的接口分层，但没有照搬 Celmux 应用 manager。Switch 新增的 `ImsClient` 将已有 IMS 与 IMSA 编解码器组合在共享 `proletarius-qmi-core::Client` 上；成功 open 后由该 wrapper 持有两个 service handles，第二个 service 分配失败时第一个 handle 自动释放。Close 明确尝试释放两个 ID；响应必须匹配 service/message/control flag 并通过 QMI result 检查。测试使用 fake transport，不执行设备 I/O。

本轮复核的参考 `crates/qmi/services/src/ims.rs` SHA-256 为 `7adde3796c4f76bc58bd611ccda1d14c20ca851d66bd8093bfb003f47956abc3`。该快照没有 Git metadata，不能给出可核验 commit；来源对象由此哈希固定。

该接口目前只覆盖 Go 的 QMI probe/set 路径中 IMS/IMSA 使用的操作。Go QMI enhancement 仍在 `-backend=qmi` 才会启动、默认 AT 路径保持不变；其 operator 查询还依赖 NAS MCC/MNC 与 operator name，目前没有迁移。`switchd-rs` 也尚未持有该 QMI owner，因此此项只为下一步 Rust-only owner 集成打基础，不增加生产迁移进度。Celmux manager 的 NAS/UIM/WMS/WDS 等 service 全量初始化、自动 rebind/replay、Linux QRTR、Windows USB 和 data-plane 与 Switch 当前的调用范围不等价，暂不移入。

## 2026-10-05：Celmux NAS operator-name query 适配（Iteration 073）

增加 NAS service ID `0x03`、Get Operator Name message `0x0039` 及 `NasClient`。响应先通过统一 QMI service/message/response-flag/result 校验，再按 Go `quectel-qmi-go` 相同字段规则解析 TLV `0x10`（跳过首字节）和 `0x13`；`preferred_name` 保持 SPN 优先、operator string 回退。复用了 Celmux QMI services MIT crate 的接口和字段映射，但没有复制其大段全功能 NAS codec 或 manager。来源 `/Users/proletarius/Downloads/celmux-src/crates/qmi/services/src/nas.rs` SHA-256：`a02dc34827e732820b69be619bfdb201fd16ddbff5cb9cdfcafeb36357141f96`。

## 2026-10-05：Celmux UIM home MCC/MNC 只读查询（Iteration 074）

新增窄版 `UimClient`，复用 Celmux `uim.rs` 的 UIM service `0x0b`、`READ_TRANSPARENT` `0x0020`、`READ_RECORD` `0x0021`、`GET_FILE_ATTRIBUTES` `0x0024` 和 session/file TLV 布局。它读取 EF-OPL（`0x6fc6`）有效精确 PLMN；没有可用条目时按 Go 现有规则回退 EF-IMSI（`0x6f07`）与 EF-AD（`0x6fad`）MNC 长度，路径尝试次序、EF-IMSI首个身份类型 nibble 处理、2/3位 MNC 和2位缺省均保持兼容。NAS 当前 serving PLMN 仍不替代 home MCC/MNC。没有接入 Celmux 的 SIM manager、卡片写入/PIN、全量状态与平台 runtime。来源文件 SHA-256：`9c8f74ffc2e78ef0d57c5ef2dfe559502581d60ced815d140ee0db35f5b8e4a5`。

定向测试覆盖 EF-OPL 记录优先级、服务 ID/消息/TLV 路径、2与3位 MNC 回退和解码边界，均使用 fake QMI transport。UIM client 尚未被 `switchd-rs` 调用或连接真实设备；QMI 三类 facade 仍不代表生产 owner 接管，总进度按加权口径保持 **20.0%**。

## 2026-10-05：复用 Celmux 的传输无关 SIP 核心

从快照中仅接入 `crates/sip/core/src/lib.rs`、`dialog.rs`、`transaction.rs` 到 `rust/crates/proletarius-sip`。三文件逐文件 SPDX 均为 MIT，文件版权头保留；新增 crate 不含 Celmux 的 `auth.rs`、SIP socket/runtime、IMS service、VoWiFi 生命周期或 UI。源快照没有 `.git` 元数据，因此对这三份源文件使用 SHA-256 固定审阅对象：`lib.rs e52b4d917da2b571079668f6b041fddd1747ea0d4ed2503c39ba8003da3be15d`、`dialog.rs ff8b68d2911e99d8b03fa1c00b8d79c6d4d576d3be4a5b9c517d5c9c37948b0b`、`transaction.rs 07fc76aa035cc0e8aa32dc27dd212d3567969514b5024024b10efcedddb67563`。Cargo manifest 对完整 Celmux crate 声明 BSD-2-Clause，但本次只采用文件头明确标 MIT 的三个文件；不复制的 `auth.rs` 文件为 BSD-2-Clause，也不将 manifest license 当成源文件 license。

新 crate 的四项离线 contract tests 覆盖 SIP 字节流被拆分/合并时的边界、keepalive 与 body framing、冲突 Content-Length 和大小上限、dialog route-set 顺序及 ACK/BYE CSeq、INVITE client/server 的 CANCEL/ACK 与事务 timer。它还未成为生产 SIP parser/dispatcher；新建协议基础不代表 Go call/SMS/IMS owner 已迁移，也不单独增加整体进度。后续用 Switch/Go 脱敏 golden traces 校验，再将相符事务接入 Rust 应用 owner。完整 MIT 条款见 `THIRD_PARTY_NOTICES.md`。

## 2026-10-05：LPA reader 组合入口

新增 `rust/crates/proletarius-reader/src/euicc_lpa.rs`，将 Switch `ApduTransport`/owned CCID logical channel、`proletarius-euicc::Euicc` 和 Celmux-derived `proletarius-lpa::Client` 组成一个 caller-owned client API。网络实现从 Switch 的 `Es9Transport` 注入，STORE DATA MSS由调用方明确传入：Standalone reader传240字节，模块/QMI可保留120字节。Fake APDU 测试验证client组合、MSS选择和SELECT失败清理；该轮针对 reader/LPA/eUICC 三个crate的52项测试以及workspace 738项测试通过。helper不拥有USB/BLE设备或Go跨进程租约；在Rust daemon下载服务、operation确认/取消、lease续期与release、错误/结果对照接通前，不算生产接管或进度增长。具体记录见 [Iteration 060](../iteration-060-progress.md)。

AT transaction 的有限响应收集、逐行 OK/ERROR/prompt 终态匹配已适配进唯一 Rust modem actor；另已将 MIT eUICC/CCID 协议 crates 局部复制到 Switch 并完成离线 mock 验证。完整代码来源、SHA-256、许可和当前不可调用的产品边界见下方 2026-10-05 复核记录。参考目录没有改动；本次没有启动 App、打开读卡器或进行真实短信、Keychain、网络操作，也未生成 DMG。

## 2026-10-05：新提供源码复核与首次可复用模块接入

源目录 `/Users/proletarius/Downloads/celmux-src/` 不含可核验的 `.git` revision，以下哈希固定本次读到的确切文件。只复制两个文件级 MIT crate 到 `/Users/proletarius/Desktop/Switch/src/rust/crates/`，保留 `Copyright (c) 2026 anti-rainer` 与 `SPDX-License-Identifier: MIT`，仅把 `celmux_ccid` crate 引用重命名为 `proletarius_ccid`。参考文件哈希：

| Celmux 源路径 | SHA-256 |
|---|---|
| `crates/euicc/ccid/src/apdu.rs` | `30b5e6b771d2c9afcc085f7af8ffb14cdee01689311fd2cb9cc0fe9c2a912d5e` |
| `crates/euicc/ccid/src/bertlv.rs` | `3cd8735b3b560702aaced2a3e671c3760461c7f48b502215e9a6414e15baf1b7` |
| `crates/euicc/ccid/src/ccid.rs` | `7ba875904781a7f430c2c893581055790b25b92c0ed15f75fd9e378062721f12` |
| `crates/euicc/ccid/src/channel.rs` | `1cf2d8860b0164c746de254cfd6f769b01d70b4c8978e0b50fb23dc31dcc20ad` |
| `crates/euicc/ccid/src/lib.rs` | `03b294d03ca62aa604ce606104720f7751a5a7a4468911452d3a3931b2dfca31` |
| `crates/euicc/ccid/src/t0.rs` | `8336ce0068ca6aaf3bfd2d7b726897c85fa3620b9928826dc61a3763edde61c3` |
| `crates/euicc/ccid/src/t1.rs` | `d773b444567037ae94ef1af4f5d0ab23b8f4e43ecab8def163297bdbad414868` |
| `crates/euicc/euicc/src/lib.rs` | `59eb511003a8bd8b3e12b4837debbfe4e93644af3a4b908049d0a5bf3137c538` |
| `crates/euicc/euicc/src/profile_package.rs` | `e51c6e8bbe0b8bf1f85d0710862472e8aba96c2643d1a677f07bc363d846cdb5` |
| `crates/euicc/euicc/src/rsp.rs` | `0c6a19fcea3e337ada0e76956468863a7da50a83396f3281ab3979448dee9f57` |

Switch-only `proletarius-reader::euicc_transport::ApduCardTransport` adapts the existing `proletarius-sim::ApduTransport` to the copied `CardTransport` interface; it bounds APDU replies at 4098 bytes, clears stale output on failure, and opens no reader. The initial offline validation passed: 3 CCID BER/APDU boundary tests, 3 eUICC ICCID/profile-list transactional tests, 2 adapter tests plus 18 existing reader tests, and `proletariusd` 304 tests. Rust formatting and `git diff --check` passed. Clippy was not available in toolchain 1.98 (`cargo-clippy` component absent); this is recorded as unavailable, not passed. No application route uses these crates yet, no card command was issued, and this work does not resolve 9eSIM `6982`.

The import reuses 5,055 lines of APDU/TLV/ES10c/BPP parsing and card-operation source. It does **not** remove any Go production logic: Go remains the current eSIM owner. Net schedule savings are therefore not deducted from the 47.5 person-week baseline until Switch routes and device behavior are actually migrated and verified.

## 2026-10-05：eUICC 通道生命周期接缝复核

复看 `crates/euicc/ccid/src/channel.rs` 后，Switch 沿用它的 `CardTransport`、`OwnedChannel` 和 ISO 7816 CLA 绑定实现；但 Celmux 的 `OwnedChannel::open` 会先探测并关闭 1–3 号逻辑通道，也会在逻辑通道 SELECT 失败时回退到基本通道。Switch 原 Go CCID 路径只申请新通道、选定传入 AID，失败时关闭本次申请的通道。为了维持现有产品行为并避免关闭其他客户端遗留或正在使用的通道，Rust 新增了 `OwnedChannel::open_selected` 和 reader APDU adapter `open_euicc_channel`：只分配一个新通道，SELECT 失败时只关闭该通道，成功后 RAII 负责结束时关闭。它复用了 MIT 协议 crate 的安全通道包装，不照搬会改变外部卡状态的预清理/fallback策略。

新增的纯 fake-APDU 回归验证 MANAGE CHANNEL → SELECT AID → CLA 绑定 → 本通道关闭顺序，并验证 SELECT 失败返回原始状态字且清理本次 channel。`cargo +1.98 test --offline -p proletarius-reader` 通过22项。该抽象尚未接到公开下载 API，也尚未在 `proletariusd` 持有 Go reader lease 的条件下执行；因此没有增加17.5%的生产迁移进度。下一步仍是把 Rust LPA operation、lease续期/释放、用户确认等待、下载终态与 Go 结果差分接成一条受控生产路径；在该端到端闭环前，LPA 不计为已接管。实体卡与 9eSIM `6982` 均未触碰/处理。

## 2026-10-05：Profile 启用路径复用边界

本轮使用 Celmux 已复用的 `proletarius-ccid` / `proletarius-euicc` 处理本地 APDU 与 Profile 状态切换；没有从 Celmux 再拷新文件。保留 Switch 当前选中 reader 的 USB/PCSC owner、AID 策略，以及 CAT_BUSY 最多 3 次、每次间隔 800 ms 的重试行为，不沿用 Celmux 高层运行时的不同重试策略。切卡请求现可进入 Rust route，但 Go 仍提供跨进程 mutation lease、VoWiFi 冲突协调和 ESTKme 复位/会话重建/结果缓存收尾。因此此次扩大 Rust 执行范围，没有消除 Go 依赖。完整实现与测试证据见 [Iteration 066](../iteration-066-progress.md)。

## 2026-10-05：按用户新提供的 celmux-src 快照复核剩余可复用项

本次读取 `/Users/proletarius/Downloads/celmux-src/`，没有改动参考文件。目录不含 `.git`，因此没有可验证的 commit 或 branch；此轮以文件 SHA-256 标记关键审阅对象。项目根许可证是 PolyForm Noncommercial，但各文件另有 MIT、BSD-2-Clause 或 PolyForm SPDX，必须逐文件判断。已有复用记录与该目录中的协议文件一致；没有把“Rust 写成”当作可以整包复制的授权或技术理由。

| 剩余参考 | 对 Switch 的价值 | 决定 |
|---|---|---|
| `crates/at/src/session.rs`（SHA-256 `79a97e3863bcc90eadc3dd7bfba186477caa4a66343c22532b6201b5a74933e6`，PolyForm） | worker 单 reader、命令/URC 分流、超时及 prompt 的实现经验与 modem actor 问题相似 | 保留行为参考；Switch actor 已有代次栅栏、跨事务 SMS/存储原子性和持久归档约束，复制整套 worker 会形成第二个 owner，且不节省业务接线 |
| `crates/vowifi/session/src/liveness.rs`（SHA-256 `1fd357d7f99431929542a9e82064aad5319b178a678d32939cbb36276230616d`，MIT） | 可做 NAT keepalive/DPD 间隔与失联阈值的策略参考 | 不替换 Switch IKE 实现。Switch `IkeAuthSession` 已把探测绑定到受保护 IKE Informational exchange、双方 SPI、message-id、请求响应匹配与 SA 关闭；Celmux 的 caller-clock 策略对象不拥有这些密码学及请求并发不变量。可参考它的独立策略/快照形态补故障矩阵，不应把本地定时事件当成网络在线证据 |
| `crates/sip/core/{lib,dialog,transaction}.rs`（MIT 文件已适配到 `proletarius-sip`）与 `crates/sip/runtime/src/lib.rs`（新接入 `proletarius-sip-runtime`） | SIP framing/dialog/RFC 3261/6026 transaction 及有界 timer/事务表，不创建线程或持有 socket，可避免 P5 重写通用 SIP 事务生命周期 | 运行时已作为隔离 Rust crate 接入，但尚未被生产 IMS dispatcher 调用；先用 Go 脱敏轨迹差分，再迁移 INVITE/CANCEL/ACK/BYE。当前 REGISTER/AKA 栈有更严格的 transaction/body 校验，不应为复用而换 parser；未差分前也不计生产接管 |
| Celmux QMI services/core 与 `ccid-host` | QMI 协议已在 Switch 形成可用 crate/clients；`ccid-host` 可能提供设备发现参考 | QMI macOS libusb transport 与 clients 仍须接入 Switch Rust owner；不搬入 Celmux Linux QRTR/Windows USB/manager。`ccid-host` 源虽标 MIT，但当前 host transport 对 macOS 编译为 unsupported；Switch 已有系统 PC/SC adapter，因此不引入第二套 reader host |
| Celmux IMS/VoWiFi application/session runtime | 具有完整业务状态与托管会话例子 | 只用于架构/故障矩阵参考。Carrier/N待多 Profile/Go compatibility/Swift API 与生命周期不同；其中 PolyForm 文件的权利边界也不同于 MIT codec，整包迁入会扩大风险和验证成本 |

按节省工期排序，优先继续复用已经落地的 MIT 协议 crate（eUICC/LPA、QMI、SIP）；其次将 SIP transaction 用作完整语音迁移的成熟基础；QMI 的短期价值取决于是否继续支持现有可选 QMI backend 并接入独占 daemon owner；不建议再造 Celmux 同款 AT worker 或 VoWiFi supervisor。对每次迁移，只有接线和兼容验收通过才纳入进度及重新估算；被引用的代码行数、已有 crate 和 mock 测试不直接折算计划工期。

Celmux MIT QMI 协议组件现在已接入受限的 Rust 生产 owner，节省了 transport/client 重做成本；仍需补断连恢复和实体硬件差分。直接替换 Switch 的 AT actor、IKE liveness、IMS REGISTER parser 或 macOS reader owner，都会绕过已有业务约束。后续优先复用成熟协议 crate 并以 Go 行为轨迹/硬件边界差分，不再建设平行 runtime。Iteration 076 只增加部分 P2B 进度；固定计划基线仍为47.5人周，整体估算为20.7%。

## 2026-10-05：复用 SIP transaction runtime（Iteration 078）

对用户提供的 `/Users/proletarius/Downloads/celmux-src/` 快照再次做模块级比对后，Switch 的 `proletarius-sip` 已含 Celmux 的 SIP parser/dialog/RFC transaction primitive，但缺少可由 IMS worker 调用的有界事务注册表与 timer scheduler。现将 `crates/sip/runtime/src/lib.rs` 接入 `rust/crates/proletarius-sip-runtime`，只将 `celmux_sip_core` import 改为 Switch 本地 `proletarius_sip`；事务容量、timer 容量、事务状态转移、截止时间排序和关闭行为均保留。runtime 不开 socket、不建线程，后续可由 Switch 自有 IMS worker 作为纯组件组合。

授权按源文件头判断：被接入文件声明 `SPDX-License-Identifier: MIT`，尽管 Celmux 对应 crate manifest 标注 BSD-2-Clause；Switch 保留文件级 Copyright/SPDX，`THIRD_PARTY_NOTICES.md` 已补齐归属。来源文件 SHA-256：`827e9c67e8821881d9dd7dc7dbf807b5766e3a125e559a12f4956b905b177044`。Celmux 快照没有 `.git`，故不声称对应可验证 revision；参考目录未修改。

新增 4 个单元测试：timer reschedule 不重复占位；INVITE 定时器触发 retransmit 且临时响应取消 A timer；达到事务容量时不残留半个 transaction；关闭清空状态且拒绝新操作。`cargo +1.98 test --offline -p proletarius-sip-runtime` 4/4通过；`cargo +1.98 test --workspace --offline` 全 workspace 通过，`proletariusd` 348/348通过。原有 `native_vowifi_session.rs` 两条 dead-code warning 仍存在，与此次移植无关。

该 runtime 是 P5 的可复用基础，不是公开路由或生产通话 owner；Go 脱敏会话轨迹、IMS call/SMS 路由、RTP/音频桥及设备生命周期尚未接线和验收，因此本轮不提高 21.1% 全 Rust 加权进度，也不因复制 406 行来源代码减少 47.5 人周基线。下一步优先接 P5 前继续完成 P2B；P5 开始时可在已复用的 SIP core/runtime 上搭 Switch 专用 dispatcher，并先做 Go trace 差分。

## 2026-10-05：按 Iteration 080 复核 Rust eSIM 下载生产接线

静态核对当前代码后，修正本评估前文中过期的“LPA 尚未接下载 API/operation/reader lease”判断。`main.rs` 将当前选中的 USB/PCSC 且有卡 reader 的下载请求分派给 `reader_esim_download::start`；BLE 选择仍回退到 Go。Rust 入口先检查 Go operation、请求关闭读卡器 VoWiFi、取得 Go 提供的 generation-bound reader lease，再在 Rust journal/operation 中启动有界下载任务。worker 使用 Rust `PcscSession`、`open_lpa_client`、`HttpEs9Client` 和 `proletarius-lpa` 完成卡片 LPA；进度/包大小进入 operation snapshot，确认与取消通过 Rust handler 处理。lease heartbeat 丢失会触发取消；任务结束后先释放 lease，释放成功才请求 Go 刷新旧 reader identity cache。

因此，MIT eUICC/LPA 协议代码不只是 mock 底座，USB/PCSC 正常下载已是 Rust 的部分生产 owner；截至 Iteration 083，整体迁移账按 P2B 33.7%、22.0% 计算。未完成部分包括 Go reader lease 与 VoWiFi 冲突桥、BLE 路径、切卡/下载以外的兼容 owner 收口，以及真实卡片/运营商验收。本文更早的 Iteration 057–060 条目记录了当时尚未接通的阶段，保留作历史，不代表当前状态。此次目录审阅和文档校正没有改写 Celmux，也没有运行真实硬件或生成 DMG。

## 2026-10-05：复用 COPS 查询行为并将 operator projection 移入 Rust（Iteration 081）

复核 Celmux `crates/at/src/modem.rs` 的 serving operator 解析行为后，把 Switch `QueryOperator` 的固定命令序列 `AT+COPS=3,2` → `AT+COPS?` 和第一段 quoted PLMN 提取迁入 Rust typed-query。Celmux AT 源文件标为 PolyForm Noncommercial，因此只将其作为行为参考；Rust parser 是 Switch 自有实现，没有复制该文件。Go 保留本地 PLMN 显示名映射、蜂窝探测抑制与无 Rust owner 的旧路径。伪串口覆盖格式选择返回 `ERROR` 后仍执行读取，Go 单测覆盖映射/飞行模式抑制，Unix UDS fixture 覆盖 Rust adapter 回包。该项让一个现有 modem 查询的执行与解析归 Rust actor，但没有设备或运营商验收。

## 2026-10-05：COPS 接入制式回退查询适配（Iteration 082）

在 Go-compatible typed-query 中新增固定 `network_mode_fallback` 名称，Rust actor 只执行 `AT+COPS?` 并按 Switch 既有表映射 AcT 0/2/7/10。它替代 Go 在 `QNWINFO` 空结果后的裸 `AT+COPS?` 读取，不接收任意 AT 文本；Go 的主查询优先级、错误保留、飞行模式抑制及 legacy port fallback 均保持。此项是 Celmux AT 命令行为的继续适配，不复制 PolyForm AT 源码，也未对实体模块发起查询。离线回归、route census 与进度依据见 [Iteration 082](../iteration-082-progress.md)。

## 2026-10-05：Rust modem actor 接管 SIM EF-SPN 查询（Iteration 083）

将 Go 固定 `AT+CRSM=176,28486,0,0,17` 读取迁入既有 Rust actor，并按 Go 样本实现完整 EF-SPN 解码，包括 GSM、ASCII、标准与压缩 UCS2。没有依赖 Celmux 应用层；该步骤继续采用其“单设备事务 owner”的行为原则，保留 Switch 的固定 typed-query 边界。此项无真实卡I/O，详细parser测试与进度见 [Iteration 083](../iteration-083-progress.md)。

## 2026-10-05：按新提供的 `celmux-src` 做全量可复用性复核

复核路径为 `/Users/proletarius/Downloads/celmux-src/`。该目录没有 `.git` 元数据，不能把它绑定到某个可验证 commit；本记录只依据本地文件内容与 SHA-256。参考目录保持只读。Celmux 的 workspace 设计与 Switch 的 Rust 后端目标高度相近，但目录/语言相同不代表上层组件可原样替换；优先级按“能减少当前迁移工作且不改变 Switch 产品契约”判定。

| 可复用方向 | 当前判断与行动 | 对 Switch 的适配边界 |
|---|---|---|
| eUICC / LPA | 已复用文件级 MIT 的 CCID/APDU、ES10c profile 解析和 SGP.22 LPA 状态机；USB/PCSC 下载和本地 profile 操作已经由 Rust 路由接手。 | 不再建第二份协议实现；保留 Switch 自己的 PC/SC adapter、Go generation-bound lease、用户确认和 durable operation journal。BLE 与 Go 侧冲突协调仍未迁完。 |
| QMI wire / client / services | 已复用 MIT 的 QMUX/QMI framing、client transaction core 和 IMS/IMSA/NAS/UIM codec，接入 macOS Rust USB owner。 | 目前设备面限 control-only 且由 Go 兼容进程选路。Celmux QMI manager 全包含 QRTR/Linux、WDS/WMS、数据面与广泛 indication，不能作为可直接替换的 macOS 控制 owner。 |
| QMI owner 有界恢复（Iteration 084 已适配） | `crates/qmi/manager/src/lib.rs` 与 `services.rs`（文件 SPDX MIT）提供 transport reopen/backoff、service handle 错误分类等参考；其中 recovery 合并、独立 service rebind 与 indication replay 属于 Celmux 更完整 manager 能力。 | Switch 已实现窄范围的单 owner 恢复：transport loss/stale handle 立即重开；单 operation scope 在60秒内3次 timeout 才触发；只读查询恢复后最多重试一次；结果不确定的 IMS 写请求永不重放；初次 open 与断线 reopen 均保留 control path 并受500ms至30秒指数退避限制。fake session 14项覆盖恢复、失败/backoff、close 与不重放。没有复制 Celmux manager，也未引入 recovery 合并、服务级局部 rebind 或 indication replay。 | 这项已计入P2B，但仅为离线策略证据。Go仍控制 QMI backend 模式，真实USB拔插、服务handle与运营商差分尚未验收；不可据此宣称完整QMI设备owner迁移。 |
| SIP | Celmux file-level MIT parser/dialog/transaction primitives 与 timer runtime 已进入 Switch `proletarius-sip` / `proletarius-sip-runtime`。 | 事务库尚未连入生产 IMS dispatcher；在替换 Go 前仍需对照脱敏 SIP trace，覆盖 ACK/CANCEL、重传和来电/挂断竞争。 |
| IMS 短信与通话 | Celmux `crates/ims/src/sms.rs`、`registration.rs`、`call.rs`、`call_session.rs`、`incoming_call.rs`、`sdp.rs` 等文件声明 MIT，可作为 P3/P5 组件候选；这些比整包复制更可能节省 Switch 的 IMS dispatcher/通话状态机工时。 | 按文件头核许可：同一 IMS crate 的 `service_runtime.rs`、`endpoint_session.rs` 标为 PolyForm Noncommercial，不能由 crate manifest 的 MIT 标签推断整包都是 MIT。Switch 已有 REGISTER/AKA/SIP/RTP/SMS 协议逻辑；逐项先做 Go trace 行为差分，保留身份代次、先归档后 ACK、设备路由及 Swift Audio 边界。 |
| VoWiFi / IKE | Celmux `vowifi/core` 多数纯 IKE/EAP-AKA/IPsec 文件为 MIT，可作标准向量和状态转换差分来源；Switch 已有自己的 Rust IKE/ESP 会话与更具体的多 Profile 生命周期约束。 | 不替换 Switch 的 SPI/message-id、身份绑定、重放窗口及受保护探测逻辑。`vowifi/session` 虽在 manifest 写 MIT，但 `managed.rs` / `start.rs` 文件级 SPDX 为 PolyForm Noncommercial；`ims/vowifi/runtime.rs` 也为 PolyForm。保留逐文件许可边界，不移植 Celmux orchestrator。 |
| RTP 与 codecs | MIT 的 `media/core` RTP、EVS framing/FMTP 和 Opus 封装可作为 P5 互操作对照，可能减少新状态机/解析器开发。 | 当前 Switch 的 P5 仍需把媒体切换、CoreAudio、RTP 时钟和会话终止接线；Celmux 的 native EVS 绑定为 PolyForm，AMR-WB 栈另有 Apache/专利约束。不能因源码许可就认为商业/分发专利权也已解决，编解码集成单独验收。 |
| 桌面端、Web、数据库与服务总装 | 不复用。Celmux 提供 Vue 和 SQLite；Switch 既定保留 SwiftUI 与当前配置/JSON/队列语义。 | 不引入第二个 UI、schema、数据库迁移或服务器生命周期。只借鉴独立 Rust crate 与 transport 注入的分层方式。 |

### 本轮结论与执行顺序

1. 先复用已经进入 Switch 的 MIT 协议组件，不重复实现相同 codec；已有组件完成产品接线与差分验收后再折算 Rust 后端进度。
2. 硬件迁移近期的最高候选是 Celmux QMI manager 的小范围恢复能力，而不是把整套 manager 搬进 Switch。当前 `QmiEnhancement` 持有一个控制面 session；成功恢复必须同时处理 service client id、设备拔出、USB owner 释放与 `close` 并发，mutating RPC 必须保持不自动重试。
3. P3/P5 优先筛选 MIT 的 IMS SMS/通话/RTP 纯逻辑，再以 Switch 自有归档、会话/设备 owner 和音频桥组装；涉及 PolyForm 文件时只做行为参考，不复制实现。
4. 每次引入都保留来源文件 SPDX/版权，并在 `THIRD_PARTY_NOTICES.md` 补充来源。Celmux manifest 与单文件 SPDX 冲突时按具体源文件处理；编码器的专利许可另行审查。

本轮没有从 Celmux 复制新的源码。Switch 自有的 QMI owner 恢复实现已完成，并通过 fake session/factory 故障注入；因此按实施与离线验证证据，将总进度调整为 **22.0%（10.467 / 47.5 人周）**，P2B **33.9%**。这不代表 QMI 实体设备恢复已验收。

## 2026-10-05：QMI owner 恢复策略落地（Iteration 084）

按本复用审阅的建议，只借鉴 Celmux MIT manager 的错误分类和重开生命周期，不复制其 manager。Switch 保持自己的单一 macOS libusb control owner、IMS/NAS/UIM typed API、错误 DTO 和 Go callback 兼容边界。传输断开/失效服务句柄立即恢复；相同 operation scope 在 60 秒内累计 3 次 timeout 才恢复；读取恢复后只重试一次；不确定的 IMS 设置写入绝不重放；owner 重开失败使用封顶指数退避。来源快照 SHA-256 及适配边界见 [`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md)，实现及测试见 [Iteration 084](../iteration-084-progress.md)。

该方案保留 QMI 请求可能已在设备侧执行的事实，不把“重新打开 transport”当成写入幂等性证明。后续只应在 Go QMI 模式编排迁移、实体 USB 故障差分和硬件矩阵中继续推进，不能自动扩展为重放任意控制请求。

## 2026-10-05：组合根运行时与启动 readiness 复核（Iteration 085）

按用户补充的本地 Celmux 快照重新检查 `crates/runtime/src/lib.rs`。该 crate 将 Control、IMS、Media 作为显式调度 lane，以 wake/deadline 驱动 `RuntimeHost`，并统一负责线程收停与 join；文件 SHA-256 为 `fca3a73d63e43765ba320e8668fec2df05dd9e14d45fd3039b06fe8ed5c4ba22`。manifest 声明 PolyForm Noncommercial。Switch 已使用 Tokio daemon 和显式 actor，直接叠加 Celmux scheduler 会重复调度，并跨入不同 crate 许可边界；本轮只采纳分层原则，没有复制 Celmux 运行时代码。

Switch 已有 Rust `/api/v2/runtime`，公开 `ok` 与独立的 `go_backend_connected/phase`；原先 Swift `BackendReadiness` 却用 Go-owned `/api/health` 决定整个后端进程是否启动。Iteration 085 将启动探测改为 Rust runtime 路由，Go-owned `/api/health` 保留设备快照和功能状态用途。离线夹具专门模拟 Rust `ok=true`、`go_backend_connected=false` 并验证 Swift 后端报告运行。这个接缝改进不等于硬件就绪，也没有移除 Go 依赖。


## 2026-10-05：通知列表生产接管

复用的MIT eUICC核心新增同序号多payload提取，并修正List/Retrieve响应错误choice被当作空列表的问题；这是Switch兼容性补强，并非原始Celmux已经通过本项目卡片验收。选定USB/PCSC的通知列表/自动enable-disable投递、卡内确认现由Rust执行，保留Go的发现AID优先、失败列表、去重排序及显式AID关闭错误语义。短Go租约与BLE仍保留。代码和离线证据见[Iteration 086](../iteration-086-progress.md)，未访问真实卡片或运营商服务。

## 2026-10-05：EVS 复用边界

用户最新指令为 **EVS优先、AMR次选，不使用PCMA**，覆盖此前禁用AMR的临时政策。当前Switch主机过渡能力仅为AMR-NB mode7；EVS/AMR-WB未实现。全Rust业务目标允许复用经核验的成熟AMR/EVS FFI，不要求把codec算法从零重写为Rust。

Celmux的`media/evs` RTP/fmtp/control文件标MIT，适合作协议边界参考；`evs-native`为PolyForm Noncommercial，真实Primary算法另行下载3GPP TS26.443 C参考codec，不能误称纯Rust或全MIT实现。脚本当前不支持Darwin，后续须独立完成macOS arm64构建、双向能力自检、向量、PCM/时钟适配及通话验收后才宣告EVS。AMR-WB IO不再被政策禁止，但native失败后隐式落到未经验证的IO仍不可照搬：必须具备真实编解码器、匹配已协商能力并正确处理CMR和帧模式切换，不能吞掉Primary初始化失败。

本轮未复制或构建EVS、未做实卡通话；没有模块DSP的codec控制证据，不能把主机停用PCMA等同于模块固件已禁用PCMA。原47.5人周基线仍为22.2%，EVS新增成本待试点后重估，不以允许IO或恢复AMR增加Rust迁移得分。最新范围见[EVS/AMR评估与实施计划](2026-10-05-evs-pcma-plan.md)；文件路径保留旧名以免链接失效。
