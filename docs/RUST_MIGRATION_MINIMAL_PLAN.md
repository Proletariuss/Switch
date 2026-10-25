> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# Proletarius 最小高收益 Rust 迁移目标

## 当前决策更新（2026-09-25）：冻结迁移范围

用户已决定不再为 Rust 覆盖率继续重构。自本节起，下面记录的“必须迁移”和阶段顺序仅作为历史路线，不再构成待执行清单；与本节冲突时以本节为准。

- 当前可交付基线保持混合架构：保留已验证的 Rust 组件（网关、归档、协议编解码、传真等），读卡器 VoWiFi 实时生产 owner 继续由现有 Go 路径承担；DJI 与 eSIM 写卡继续使用 Go 硬件桥。
- 不为完成 9/15 或删除 Go 而接通尚未启用的 Rust VoWiFi actor。该路径仍缺 ESTK BLE AKA 的同一租约桥接、生产会话生命周期、SMS/通话接线，以及 CTExcel/giffgaff 真机门禁；这是一项新的高风险接管工程，不是当前迭代的收尾工作。
- 2.25 为当前软件基线。本轮确认 VoWiFi crate 的 133 项测试及 native session 定向测试 13 项通过；复验现有 2.25 macOS 27 DMG 的镜像 CRC 有效，且与 `dist/` 副本逐字节一致（SHA-256：`7e0cf88c4e4973095bb766e377d8e674a327b46308ffcfdadf1a3a90040c2771`）。本轮只有路线文档变化、没有应用源码变更，因此复用该已验证 DMG，不重复构建；未进行新的运营商真机验收。
- 暂不清理或删除被 fail-closed 门禁隔离的试验代码，也不批量删除 Go 兼容实现，以免扩大变更面或破坏当前可用链路。只有明确用户 bug、稳定性/安全缺陷或构建阻断才做最小必要修改；UI 和架构保持不动。
- 迁移指标固定解释为历史生产所有权快照 9/15（60%），不是后续重构目标，也不是整体功能完成率。停止推进该指标，不再以它作为阶段继续条件。

**收尾准则：**确认当前源代码可编译、已有定向回归通过、发布基线可复现，记录 Go/Rust 所有权边界后即结束本轮迁移目标。后续工作限定为用户明确提出的功能修复、稳定性修复与版本发布，不再安排重构迭代。

## 决策

项目不再以“重写所有 Go 代码”为目标，而采用 Rust 业务核心 + Go 必要硬件桥接的架构。Rust 接管协议、状态、归档和读卡器 VoWiFi 实时链路；Go 只保留难以安全重写且已经稳定的硬件适配。

## 必须迁移

1. 读卡器 VoWiFi 实时运行时：前置代理、ePDG、IKE/ESP/NAT-T、IMS REGISTER、SMS、SIP 通话、PCMA、DTMF、取消、重连和拔卡恢复。
2. 读卡器 VoWiFi 的短信实时生产、设备路由和本机归档。
3. 读卡器 VoWiFi 的 SIP 呼叫信令；macOS 音频输入输出仍由 Swift/CoreAudio 负责。
4. Rust 统一设备快照、revision、错误码和生命周期；页面不再自行拼接或轮询 Go 状态。
5. 已完成的 Rust 传真协议、传真会话和传真归档保持原生实现。

## 明确保留

- DJI USB/QMI/AT、模块短信、VoLTE、eSIM 和恢复逻辑暂时由 Go 硬件桥接进程负责。
- 读卡器 eSIM Profile 下载、切换、删除等写卡操作暂时由 Go 驱动负责；Rust 负责请求校验、操作锁、generation 和状态回收。
- SwiftUI、macOS 生命周期、CoreAudio、麦克风、扬声器和 AirPods 路由不迁移。

保留项必须通过 Rust 私有 Unix socket 暴露窄接口，不能直接成为公共 API 或状态源。Rust 和 Go 不得同时打开同一读卡器或 DJI 设备。

## 迁移门禁

每个实时组件依次通过协议单元测试、Go/Rust 差分测试、超时/取消/拔插故障注入、Clash 前置代理和合法直连测试，以及三轮真机循环，才允许删除对应 Go 业务代码。运营商 IMS 超时或 SIP 487 只能作为失败证据记录，不能被测试通过掩盖。

## 阶段顺序

1. 收紧 Go 为硬件桥，Rust 成为唯一公共 API 和状态源。
2. Rust 接管读卡器 VoWiFi IKE/ESP/IMS。
3. Rust 接管读卡器 VoWiFi SMS 和 SIP 通话信令；Swift 保留 CoreAudio。
4. 删除 Go 中重复的状态、归档、编解码和公共路由。
5. 按 2026-09-24 更新后的目标，当前阶段保持 UI 不变，专注通信链路、稳定性和必要 Rust 迁移；不再将 UI 风格重设计列为本目标的交付条件。

## 成功定义

“Rust 迁移完成”指所有通用业务协议和读卡器 VoWiFi 实时业务由 Rust 负责，而不是强行删除 DJI/eSIM 硬件桥。只有在未来单独完成硬件驱动迁移并通过门禁后，才讨论移除 Go 进程本身。

## 进度读数规则

页面与 API 中的 `60%` 是一个严格的**生产所有权**读数：15 个独立组件中，9 个已由 Rust 独占、无需 Go 运行时且不阻塞 Go 移除。它不是“写了多少 Rust”、也不是静态测试覆盖率，更不是用户可用功能的完成度。

下一次该读数增加，必须同时满足：

1. Rust 的 `reader_vowifi` session supervisor 已实际独占一张指定读卡器/SIM 的 IKE、AKA、IMS、安全关联和停止顺序；
2. CTExcel 与 giffgaff 分别通过代理/合法直连下的 IKE → IMS → SMS → 主叫及被叫通话真机门禁；
3. 同一卡的 Go 会话已停止，故障注入证明不存在双 owner、串卡、旧 generation 清空新状态或“界面在线、链路离线”；
4. 再由 `ownership.rs` 同步变更组件 owner、计数、测试和本文件。

在此之前，协议、配置、差分或 loopback 测试的推进会记录为“切换门禁推进”，但不会人为改写 60%。这样数字停留不代表工作停滞，而是避免把尚未真机接管的链路宣称为生产完成。

## 当前基线（2.25）

- 本阶段版本：2.25 / build 225。IMS 订户身份现在携带不可调试、零化的 SIM IMSI，并绑定解析时的 carrier/domain/realm/identity-source；native REGISTER coordinator 可直接消费 generation-bound session 已解析的身份，且在任何 P-CSCF dial 前拒绝 SIM/profile 不匹配，plain/authenticated/protected REGISTER 复用同一身份，不再重读可变 SIM 字段。既有调用仍走兼容 resolver。
- 收益/成本：新增少量私有绑定元数据与一个适配入口，避免临时重选身份造成跨卡/跨 profile 的 REGISTER 头与 AKA username 不一致；复用既有 type-state coordinator，无新依赖、无网络/DNS路径、Go endpoint、公共诊断 payload、UI 或运行 owner 变更。
- 验证：VoWiFi 133 项、daemon 161 项测试通过；动态 sec-agree loopback 测试改为将同一 resolved identity 用于初始与受保护 REGISTER；profile/IMSI 错配有类型化拒绝覆盖。真机 IMS/短信/电话未运行。
- 生产边界：native keyed session actor、IMS/SMS/Call live runtime 与运营商硬件验收仍未完成；Rust 生产所有权仍为 9/15（60%）。本阶段应按用户明确要求构建最新版 macOS 27 DMG，构建结果及校验信息记录在 2.25 阶段记录。

## 前一基线（2.23）

- 本阶段版本：2.23 / build 223。把 Go reader cache 中已有的 ISIM IMPI/IMPU 沿 generation-bound private reader lease 传入 Rust；该通道为私有 Unix socket，字段在 Go/Rust lease 内用敏感字符串处理，不进入公共快照、诊断日志或 Debug 输出。Go `derived` 身份仍不变；Rust IMS profile 新增 `derived/isim/auto` 选择语义，`auto` 仅在身份成对缺失时回退，部分身份或不安全 header 字符均 fail-closed。
- Rust REGISTER profile 单元覆盖初始 REGISTER、AKA Digest username 与 ISIM IMPU，并检查 scheme/header 注入；动态 loopback 注册用例验证 AKA 后和 sec-agree 后 REGISTER 均携带预期 ISIM 身份。Go lease 与 Rust deserialization 契约另有定向测试。
- 生产边界：ISIM 值已随 lease 进入 Rust，但当前 keyed native VoWiFi actor/start route 尚未消费这两个值，故本次没有切换 IMS production owner，也没有改变既有 Go 会话行为。没有真实 SIM/ePDG/IMS、短信或通话验收；Rust 生产所有权仍 9/15（60%）。
- 成本取舍：复用既有 reader lease、profile compiler 和 REGISTER coordinator；仅启用 zeroize 的 serde 支持，没有新增第三方依赖、公共 API、DNS/代理策略或新的后台组件。此桥接为后续 actor 接管准备受保护身份输入，不把未接线能力计作生产完成。
- 验证：VoWiFi 132 项、daemon 160 项测试，Rust fmt 检查、Go reader lease/API 定向测试与 `proletariusd` Release 编译通过。正式 macOS 27 ARM64 Release/App 签名及 DMG 校验在本次打包阶段执行。
- DMG：`dist/Proletarius-2.23-macOS27.dmg`，42,336,228 字节，SHA-256 `9149b6c5c261fed1dd51bac6cb84f3b1546f7de948cb839fb3e2aabe4c8d69c7`；正式 macOS 27 ARM64 Release/ad-hoc 签名检查和 `hdiutil verify` 通过，副本保存于桌面 `test` 文件夹。不使用 UPX。

## 前一基线（2.22）

- 本阶段版本：2.22 / build 222。把原生 carrier profile 编译器接到已有的 IMS 注册协调器：初始 REGISTER、AKA challenge 后的 plain authenticated REGISTER、以及 sec-agree 后的 protected REGISTER 都由同一份已解析运营商 profile 生成，并沿用协调器对明文/受保护连接、SA 安装和资源回收的顺序约束。
- 新增 `register_native_profile` 适配入口，SIM AKA 仍由调用方通过受控回调提供；profile 编译错误与网络/清理错误分别保留，不通过“默认值”吞掉运营商配置错误。它尚未接入 production keyed session actor 或公共 start route，Go 生产会话 owner 未变化。
- 动态 loopback 回归使用真实 carrier registry 的 Orange UK/CTExcel profile，经两个 userspace netstack 执行 plain REGISTER → 模拟 SIM AKA → sec-agree → protected REGISTER，并验证同一受保护关联上的合并到达 INVITE、响应路由和 SA/端口回收。此项证明原生 profile、IMS coordinator 与 userspace netstack 的组件接线，不代表真实 ePDG、SIM、运营商 IMS 或 SMS/通话验收。
- 成本取舍：复用现有 carrier profile compiler、REGISTER 状态机和隧道 netstack，仅增加窄的请求构造适配层；没有新增依赖、DNS 路径、公共 API 或 Go 行为分叉，降低模板字段在 Rust 注册请求中的重复映射成本。keyed live actor、生产 session 路由、短信/语音双向链路及 CTExcel/giffgaff 真机门禁仍未完成，生产所有权保持 9/15（60%）。
- 本阶段 DMG：`dist/Proletarius-2.22-macOS27.dmg`；发布副本保存至桌面 `test` 文件夹。macOS 27 / Apple Silicon；不使用 UPX。只在格式、定向与 daemon 测试、Release 编译和镜像校验均通过后标记构建成功。

## 前一基线（2.14）

- 本阶段版本：2.14 / build 214。核对 Rust 原生会话的实际 AKA 传输后确认：`ReaderAkaBinding` 直接调用 Rust PC/SC 读卡；Go 的 `openReaderTransmitter` 才能按 reader 类型选择 PC/SC 或 ESTK BLE bridge。Rust 尚无可在持有同一 reader/SIM lease 时调用的 BLE AKA provider。此前原生入口 gate 只列出跨进程 lease、actor、受保护 IMS 与实时 SMS/Call 四项，没有单独暴露这一硬件适配缺口。
- 修正：native owner readiness 新增 `ble_aka_transport_bridge` 必备门槛；当前和之前一样始终 fail-closed，入口不会因此开始触卡或发包。测试断言该门槛缺失时仍返回冲突，并逐个证明任一门槛缺失都会拒绝启动。
- 成本取舍：此改动不是迁移完成度，也不增加新的 BLE API/协议或卡操作；它阻止未来仅因 PC/SC actor 可用就错误地启用 ESTK BLE session。下一项高收益工作应把 generation-bound lease、Go 私有 USB/BLE AKA bridge 与真实 native actor 一起接通，而不是分开累积无消费者的接口。
- 生产所有权仍为 9/15（60%）；没有运行 CTExcel/giffgaff 真机网络，也没有输出 DMG。

## 前一基线（2.13）

- 本阶段版本：2.13 / build 213。短信业务传输仍由 Go IMS session 持有，但短信 wire codec 已固定由 Rust gateway 提供：读卡器 SMS 的 TPDU 分段、RPDU 封装/解析、SMS-DELIVER 与状态报告解析不再因 Rust 私有 socket 缺失而静默退回 Go 的 `device` / `messaging` 实现。gateway 未就绪时返回明确错误，避免不同编码器造成可变结果；不影响模块 SMS、读卡器 SIM/APDU 或 IMS 网络传输。
- 生产路径依据：Swift 默认启动 Rust `proletariusd-rs`；Rust gateway 启动 Go 硬件桥时显式注入 `PROLETARIUS_CARRIER_SOCKET`。故 Rust socket 缺失代表服务启动/部署异常，应 fail-closed，而不是启用第二套短信实现。
- 验收：Go bridge 四类编解码调用在无 Rust socket 时均明确失败；Go→Rust 请求 envelope 单元测试、Rust SMS Submit 与 Go codec 对照（覆盖 888/65075/国际号码、GSM 扩展字符、中文、emoji、多段边界）通过；Go IMS/device/messaging 定向测试通过。没有连接运营商、读卡器或发送短信。
- 成本取舍：删除 Go 读卡器短信 shim 的隐式编解码回退，不删除共享 Go modem SMS 代码，也不迁移 SIP MESSAGE/归档生产者。收益是生产环境只有一套 SMS wire codec 且桥接异常不再改变实现；本阶段并未改变 live IMS/SMS 会话 owner，生产所有权仍为 9/15（60%），不能宣称 SMS 端到端 Rust 化。
- DMG：`dist/Proletarius-2.13-macOS27.dmg`，Release ARM64、最低 macOS 27.0；生成后同步复制至桌面 `test` 文件夹，并校验签名和镜像 CRC。未使用 UPX。

## 前一基线（2.12）

- 本阶段版本：2.12 / build 212。Rust 现在不仅选择 Apple carrier bundle，还负责将模板展开为运行 profile；启用 Rust 本机服务时，Go IMS 桥直接消费该规范化结果，不再把同一份 Apple JSON/YAML 再解析、展开一次。`PROLETARIUS_CARRIER_SOCKET` 未配置的 Go-only 恢复路径与既有 `carrier-overrides.yaml` / UI 自定义 ePDG 覆盖保持原样。
- 差分门禁：Rust 生成、Go 比对的 2,722 个合成 SIM/selector 样本逐字段一致；测试已加入 `scripts/test-migration-gates.sh`。Rust/Go 全套迁移门禁和 PCMA、IMS SDP、AMR 差分均通过。该步骤没有访问运营商网络或改变 SIM。
- 迁移边界：该配置解释层已切到生产 Rust 路径，但 IMS REGISTER、IKE/ESP session 生命周期、SMS 与 SIP 通话 owner 仍由 Go 持有；尚未满足指定号卡的完整真机闭环，因此生产所有权仍为 9/15（60%）。这不是整体项目完成百分比。
- DMG：`dist/Proletarius-2.12-macOS27.dmg`，Release ARM64，macOS 27.0 起；构建与签名/镜像校验完成后同步保存至桌面 `test` 文件夹。未使用 UPX。

- 本阶段版本：2.11 / build 211；Rust workspace 格式检查与测试、Go `vet`/兼容测试、Rust↔Go PCMA/IMS SDP/AMR 差分门槛全部通过。
- macOS 27.0 arm64 Release 构建、嵌入式 Go/Rust 后端深层签名检查、DMG CRC 校验均通过；产物为 `dist/Proletarius-2.11-macOS27.dmg`。未使用 UPX。构建出现 macOS 27 API 弃用警告，未阻断构建。
- 真机/网络基线：本机接入 ESTKme-RED；Rust 原生只读探针成功发现 USIM AID，验证 ICCID/IMSI 与 GID1/GID2/SPN 数据形态，且不输出订户值。当前 Surge 系统 HTTP/HTTPS 监听为 `127.0.0.1:6152`、系统 SOCKS 开关关闭；Rust 从 Surge 生效配置发现 SOCKS 服务并完成 UDP ASSOCIATE，系统代理选择器返回已验证的非直连通道。此轮未连接 ePDG，也未发送 SMS 或发起呼叫。
- 本阶段没有把 Go-owned VoWiFi 实时链路切换到 Rust；生产所有权保持 9/15（60%）。该读数须等指定 SIM 的完整运营商真机闭环及隔离故障注入通过后再更新。
- 取舍：真实 Surge 代理发现、UDP ASSOCIATE 与只读 SIM 身份闭环均有通过证据，当前继续改写代理选择的预期收益低于增加第二套发现逻辑的维护成本；下一阶段应优先接通按读卡器/SIM 隔离的 Rust 会话 actor 及既有 IKE/IMS 运行时，避免在未解决生产 owner 接线前继续堆叠离线代理实现。

## 前序基线（2.10）

### 2.10：运营商 CBC 启动闭环、兼容提议与主动 IKE DELETE

- 基线：2.09 已补齐 CTExcel/giffgaff 所需的 CBC/HMAC 原语，但动态启动门禁仍主要证明 GCM；受限 native proposal builder 还会错误拒绝 GCM 配置中的 PRF-SHA1/SHA384；本地主动停止只释放数据面，不向已建立的 ePDG IKE SA 发送 DELETE。迁移所有权百分比也被误当成整体项目进度，不能准确表达门禁推进。
- 目标/测量：以本地真实 SOCKS/NAT-T 对端分别贯通 GCM、CTExcel `aes128-sha256-prfsha256-modp2048` / `aes128-sha256` 和 giffgaff `aes256-sha512-prfsha512-modp2048` / `aes256-sha512`；每条链必须覆盖 SA_INIT/DH、三轮 EAP-AKA、最终 AUTH、双向 ESP、坏 DPD 响应隔离、peer DELETE ACK 和应用失败资源释放。正常停止必须复用现有 transport，在同一 5 秒总预算内发送恰好一次受保护 IKE DELETE；存在未决 DPD 时先完成其响应并使用下一个 message ID。
- 实施：GCM proposal 保留既有 PRF-SHA1/SHA384 兼容，CBC 继续只接受已实现的 SHA256/SHA512，新增精确 wire 向量与拒绝测试。三套动态 startup loop 使用相同 production path 和错误注入。外部 stop 改由 IKE owner 消费，数据面收到 `ShutdownAfterIke` 后仅在 `send_ike` 成功时释放 ESP；命令队列满时同步轮询数据面，network/wake 竞态也不会抢先丢弃 tunnel。
- 验证：Rust IKE 39 项、VoWiFi 96 项、网关 106 项以及完整 workspace 测试全部通过，workspace 全 target strict Clippy 与格式检查通过；Go vet/兼容测试、PCMA/IMS SDP/AMR 差分和传真收件契约门禁通过。动态测试覆盖直接 stop、in-flight DPD、peer DELETE、GCM 及两套 CBC 运营商链路。2.10 macOS 27 arm64 Release App 已完成版本 2.10/build 210、三项二进制架构、最低系统 27.0 和深层签名校验；按约定不生成 DMG。
- 成本取舍：改动集中在 proposal、startup loop 与 IKE/data-plane 生命周期，不新建 socket、不改变前置代理策略、不引入后台任务。双方在极窄窗口同时发送 DELETE 时，暂不增加关闭后接收状态机来实现 RFC 7296 的 SHOULD 响应；现有路径仍 fail-closed。没有真机运营商证据前不切换 production owner，也不删除 Go 路径。
- 进度口径：生产所有权仍为 Rust 9/15（60%），这是 `ownership.rs` 的二进制架构指标，不是整体项目完成度。切换门禁单独报告为：协议单元测试已完成；本地动态/故障注入已完成上述 IKE/ESP 启停范围；Go/Rust 全量差分仍为部分完成；Clash/Surge 与合法直连真机循环仍待 production daemon 接线后完成；默认 owner 尚未切换。

### 2.10（继续）：Rust 原生 macOS/Surge 系统代理发现

- 基线：Rust 传输选择器只能消费调用方明确传入的代理；UI 留空时，macOS System Proxy 和 Surge 有效 SOCKS 端点仍由 Go 发现。若提前切换 Rust session owner，当前“Surge 只发布 HTTP 6152、实际 SOCKS 位于 6153”的自动代理行为会丢失，因此直接接线收益不足以覆盖回归风险。
- 目标/测量：显式代理继续具有最高优先级；空值时先读取有界的 `scutil --proxy`，启用且合法的 SOCKS 直接采用。只有系统 HTTP/HTTPS 是 loopback、Surge `system-proxy` 明确为 On，并且有效配置 `http-listen` 与系统端口一致时，才从同一配置读取本地 `socks5-listen`。命令均有 3 秒截止时间、输出上限和 drop-kill；不得猜端口、记录节点内容、修改 Surge 状态或阻断无代理合法直连。
- 实施：新增 Rust `system_proxy`，将发现结果直接交给既有 DNS/路由/SOCKS UDP 传输选择器，并在响应中返回 `proxy_source` 供诊断。loopback 判断统一支持 IPv4/IPv6/localhost；显式代理、普通 macOS SOCKS、Surge 有效配置和无代理分别保持独立来源。
- 动态验证：当前实机 System Proxy 为 `127.0.0.1:6152`、Surge system-proxy/Enhanced Mode 均开启，有效配置为 HTTP 6152、SOCKS 6153。Rust 自动发现得到 `127.0.0.1:6153`，真实 SOCKS 握手通过，自动发现结果继续进入传输选择并取得非直连模式；网关 106 项、完整迁移门禁与 strict Clippy 全部通过。
- 成本取舍：只迁移生产接线必需的发现逻辑，不移植 Go 的“自动开启 Surge Enhanced Mode”外部写操作；Enhanced Mode 未开启且 SOCKS UDP 不可用时，既有策略仍可根据真实 DNS/路由选择合法直连。该阶段缩小 Go 配置依赖，但没有接管 IKE/IMS session，因此 production ownership 仍为 9/15（60%）。

### 2.10（继续）：运营商缺省算法与生产启动链一致

- 基线：Rust 运营商解析器要求 IKE/CHILD proposal 数组必须显式存在，但 Go `standardProfile` 会先建立 AES128/SHA256/PRF-SHA256/DH14 与 AES128/SHA256 的窄基线，再叠加 Apple 模板。234/20 Hutchison 模板只显式发布 CHILD proposal，因而 Go 可启动、Rust 会在联网前误拒绝。
- 目标/测量：只有 proposal 字段缺失、`null` 或空数组时采用与 Go 完全相同的窄默认值；字段类型错误、显式多 transform、未实现算法、ESN 或 DH 仍 fail-closed。解析结果必须实际驱动同一 production startup 函数，贯通 SOCKS、NAT-T、SA_INIT/DH、三轮 EAP-AKA、最终 AUTH、双向 ESP、坏 DPD 隔离、peer DELETE 及应用失败释放。
- 实施与验证：CTExcel、giffgaff 和 Hutchison 的动态启动客户端现从嵌入的 853 个 PLMN selector / 683 个 profile 注册表取得 proposal，测试对端继续使用独立固定预期，避免同源自证。新增 234/20 缺省与畸形容器拒绝测试；Rust VoWiFi 99 项、IKE 39 项、IMS 40 项、网关 106 项以及完整 Go、PCMA/SDP/AMR 差分和传真契约门禁通过。
- 成本取舍：仅补齐 Go 已有的标准 profile 语义，不引入宽泛兼容列表，不改变显式运营商算法。该修复移除 daemon 接线前的真实兼容阻塞，但尚未把原生 tunnel 编排接到生产 API，也没有 CTExcel/giffgaff/Hutchison 真机证据，因此 `reader_vowifi` owner 仍为 Go，生产所有权仍为 9/15（60%）。

### 2.10（继续）：代理决策转换为原生会话输入

- 基线：Rust 已决定 ePDG 路由，但 `/api/v2/vowifi/select-transport` 只把结果编码成 JSON，Go 再解析；Rust 原生 session 无法取得 SOCKS 探测期间已经解析并握手验证的 `SocketAddr`，若自行再次解析/探测会产生竞态和重复策略。
- 目标/测量：HTTP 兼容接口与未来原生 session 必须调用同一强类型函数；SOCKS 域名模式携带真实握手验证后的代理地址，Enhanced Mode/合法直连不保留无用代理；显式代理优先级、自动系统代理来源、非致命代理错误及拒绝原因保持原响应契约。
- 实施与动态验证：`SelectedTransport` 将 `ProxyMode`、验证后代理地址、来源和探测错误固化为一次不可变决策，SOCKS 模式若缺少绑定地址会 fail-closed。内建本地 SOCKS TCP/UDP 对端验证域名模式和地址绑定；当前实机 Surge 的空代理自动发现经同一函数选择非直连路径。当前 ESTKme-RED 的只读 Rust PC/SC 探测同时验证 card present、完整 USIM AID、ICCID/IMSI 形状有效且不输出订户值。Rust 网关 106 项、workspace、strict Clippy、Go 兼容、PCMA/SDP/AMR 差分和传真契约门禁全部通过。
- 成本取舍：复用现有并发 DNS/路由/SOCKS 探测，没有新增后台扫描或网络连接；收益是移除 native tunnel 接线前的最后一次 JSON 回环和二次解析。此阶段仍未创建生产 tunnel owner，故不提升 9/15 的生产所有权指标。

### 2.10（继续）：不可变运营商 tunnel 启动快照

- 基线：Rust 运营商解析只能单独返回原始 Apple JSON 和算法列表；生产启动仍需由 Go 分别计算 ePDG、APN、IP 栈、IKE 本地身份、用户覆盖及 AKA challenge 模式，无法保证这些值和已验证算法属于同一次 SIM/模板快照。
- 目标/测量：同一 `ResolvedCarrier` 与 SIM 身份必须一次生成 ePDG/来源/端口、APN、IP 栈、可清零 IKE/EAP 身份、精确 IKE/ESP proposal 和 AKA 模式；自定义值明确覆盖模板，显式空值、空算法列表、非法端口、URL 形式 ePDG、未知 IP 栈或未实现算法必须在任何网络 I/O 前失败。
- 实施与验证：新增无 Debug/Serialize 的 `NativeCarrierTunnelConfig` 和显式可选 `NativeTunnelOverrides`。CTExcel 234/33 验证标准 ePDG、IPv6、`0$imsi` 身份展开及算法；覆盖用例验证自定义 ePDG:4500、APN、IPv4、checkcode 与空/非法输入拒绝。CTExcel、giffgaff、Hutchison 的完整 SOCKS/NAT-T/EAP-AKA/AUTH/ESP/DPD/DELETE 动态启动也改为消费该快照的 APN、IP 栈、proposal 与 AKA 模式。Rust VoWiFi 100 项及完整迁移门禁通过。
- 成本取舍：当前只迁移建立 IKE tunnel 必需且已有 Go 行为可对照的字段；IMS REGISTER 模板、设备 IMEI、计时器和 YAML 文件读取留在后续 session owner 接线，不复制完整 Go 配置层。收益超过新增小型强类型快照的成本，但生产 API 尚未切换，所有权仍为 9/15（60%）。

### 2.10（继续）：Rust 严格读取 carrier_overrides

- 基线：直接启用 Rust session owner 会丢失现有 `carrier-overrides.yaml`；让 Go 永久展开配置又会保留重复业务依赖。原 `serde_yaml` 已废弃，不符合长期生产依赖要求。
- 目标/测量：Rust 必须识别现有 Go 顶层及 IMS REGISTER/E911 完整字段形状，未知字段、超过 256 KiB、重复规范化 PLMN、非法枚举、legacy gate、无效 token 和 IMEI 必须 fail-closed；两位/三位 MNC 继续归一匹配，并保留“字段缺失”与“显式空数组”的差异。只有 tunnel 需要的覆盖才转换成 `NativeTunnelOverrides`，最终网络前校验仍由单一启动快照执行。
- 实施与验证：新增严格 `OverrideSet`，使用仍维护且兼容 Rust 1.64+ 的 `serde_yaml_ng 0.10`（Cargo 中保持 `serde_yaml` 本地别名），没有固化已废弃的 `serde_yaml 0.9`。Rust 现与 Go 一致校验 DNS 地址、计时器、ePDG 主机、IMEI Luhn、IMS REGISTER contact 顺序、安全机制 token、SIP 状态码集合和 E911 公网 HTTPS 边界。新增 8 个 Go/Rust 共享接受/拒绝样本，覆盖完整有效配置及未知字段、IMEI、legacy gate、私网 E911、重复 contact、重复状态码和非法 DNS 端口；两端消费同一 fixture 并全部通过。Rust VoWiFi 103 项、网关 106 项、Go 全套及 PCMA/SDP/AMR 差分和传真契约门禁通过。
- 成本取舍：E911 首版校验曾引入 `url` 及 26 个 ICU/IDNA 传递依赖，单一配置字段的收益不足以覆盖该成本，已改用工作区既有的轻量 `http::Uri` 并保持 fail-closed 公网 HTTPS 语义。Rust 尚未由生产 daemon 消费 override，不能据此删除 Go parser；必须等 native session owner 接线、动态差分与真机门禁通过。生产所有权仍为 9/15（60%）。

### 2.10（继续）：Rust 真卡运营商选择补齐 EF-GID1

- 基线：Rust 原生身份事务只有 ICCID/IMSI/MCC/MNC；giffgaff 等共享宿主 PLMN 的 MVNO 依赖 EF-GID1，缺失时 23410 会命中通用 O2 模板。此时即使 IKE/IMS 协议实现正确，也会使用错误的运营商参数。
- 目标/测量：完整 USIM AID、IMSI、可选 EF-AD、可选 EF-GID1 和 ICCID 必须在同一个 PC/SC 事务内读取；GID1 缺失不得丢弃稳定身份，存在时必须以上游 carrier registry 使用的原始大写十六进制形式返回，且日志与探测不得输出实际订户值。
- 实施与动态验证：新增 `ReaderCarrierIdentity`、自动/显式 AID 两个读取入口；EF-GID1 使用有界短 APDU，保留选择器需要的 `FF` 字节。Rust 身份 API 默认路径同步返回 `gid1`。脚本单元测试验证 `508FFFFF` 形状和同事务 APDU 顺序；当前插入的真实 ESTKme-RED 只读探测返回 `reader=present usim_aid=found iccid=valid imsi=valid mnc=optional gid1=valid`，未输出任何实际值，也未切卡或启动网络。
- 成本取舍：仅增加一个可选透明 EF 读取和一个小型结果类型，不引入依赖、不修改卡片；收益是消除 giffgaff 原生 session 选错模板的确定性风险。SPN/GID2 仍仅在真实模板要求且 Rust 能安全读取时再迁移，不为了覆盖率提前扩大 APDU 面。生产 session 尚未接线，所有权仍为 9/15（60%）。

### 2.10（继续）：Rust 真卡运营商选择补齐 EF-GID2 与 EF-SPN

- 基线：嵌入 selector 已支持 `gid2` 和 `spn` 条件，但 Rust 读卡器只返回 GID1。对于使用 GID2/SPN 区分的 MVNO，这会把有效身份当作缺失，最终只能落到显式 generic bundle 或拒绝，无法安全选取精确 VoWiFi 模板。
- 目标/测量：在已有 USIM 单事务内，以有界短 APDU 可选读取 EF-GID2 (`6F3F`) 和 EF-SPN (`6F46`)；GID 保留大写十六进制和 `FF`，SPN 跳过 display-condition 字节并只接受可验证的 GSM 默认字母表子集。缺失、畸形或未支持的 SPN 必须视为缺失，绝不猜测运营商品牌。身份 API 和原生会话 `SimIdentity` 必须携带两字段。
- 实施与真卡验证：`ReaderCarrierIdentity` 已扩展 GID2/SPN，native plan 传入 selector resolver，诊断 probe 只输出字段有效性而不输出任何订户值。脚本 APDU 测试覆盖 GID1/GID2/SPN 与 unsupported SPN 拒绝；当前 ESTKme-RED 的只读实卡探测返回 `gid1=valid gid2=valid spn=valid`。未切换 profile、未建立隧道、未发起网络请求。
- 成本取舍：增加两个可选 EF 读取和一个严格解码器，不新增依赖或修改卡片。上游运行时 selector 明确将 GID2/SPN 作为身份条件，且要求缺失时不猜测 MVNO；这个修复直接降低错误 ePDG/IMS 参数的风险，收益高于极小的 APDU 成本。生产 session owner 仍为 Go，故所有权仍为 9/15（60%）。

### 2.10（继续）：daemon 原生 VoWiFi 会话计划边界

- 基线：代理选择、Rust 身份、carrier registry 和 `carrier-overrides.yaml` 已各自可用，但还没有跨 Go/Rust 的读卡器租约。即使 plan 不发 IKE/IMS，PC/SC 读卡和真实 DNS/route/SOCKS 探测也不能与 Go 的在线会话并发进行。
- 目标/测量：保留内部 `POST /api/v2/vowifi/native-session-plan` 的准备代码，但在原生 session actor 获得跨进程 reader lease 前，公开路由必须 fail-closed，且不得解析请求、触卡、探测代理或启动网络。将来 actor 持有 lease 后，才可在不发 IKE/IMS 的前提下读取单张卡快照、匹配模板/覆盖并执行真实代理选择；任何缺失 MNC、无模板、无效覆盖、假代理或无可用传输必须在启动前失败。
- 实施与验证：当前公开路由统一返回 `native_session_owner_not_ready`，不再对现有 Go owner 产生 PC/SC 或网络副作用。内部 `native_vowifi_session` 仍保留有界 worker、原生 registry、严格 override parser 和 `select_for_session`，以供已租约的原生 actor 消费；输入 reader/ePDG/proxy 先于读卡验证，读卡忙、缺卡、超时与协议错误保留可操作状态码和重试语义。请求覆盖优先于 YAML 的定向测试、daemon 定向测试与 strict Clippy 通过；当前 UI 仍走已验证 Go session。
- 成本取舍：暂不伪造“只读就安全”的跨进程并发能力，也不以环境变量绕过 owner gate。收益是消除诊断端点碰撞真实读卡器的风险；代价是该预备端点在 actor/lease 前不可用。尚未启动原生 tunnel，故 reader VoWiFi owner 仍为 Go、所有权维持 9/15（60%）。

### 2.10（取舍）：暂不单独引入 Rust 直持 PC/SC 的跨进程 lease

- 基线：Go child 是当前唯一可同时覆盖 USB CCID 与 Swift BLE bridge 的硬件 owner；其 `authMu` 只在 Go 进程内序列化卡操作。Go 发起的短时 Rust identity/AKA bridge 已在该锁内调用，但 Rust 公开 PC/SC 路由本身没有跨进程 lease，不能当作长期 VoWiFi owner 的互斥机制。
- 目标/测量：首个原生 IKE/IMS actor 接线时，Go 必须提供私有、按 `reader + generation + epoch` 绑定的 identity/AKA lease；acquire 必须先停止并等待 Go VoWiFi 的 `runDone`、`swuDone` 和 retiring state 完全释放。lease 期间 Go 的 VoWiFi、切卡、下载、删除、通知、复位和直接 APDU 必须返回明确 busy；Rust 只能调用受限 identity/USIM/ISIM AKA，不得得到任意 APDU 或自行开启 PC/SC。拔卡、切卡、generation 变化、bridge 消失均必须让旧 epoch 失效并有序关闭 IMS→IKE。
- 取舍：当前没有真实 native actor 消费 lease，因此不提前实现该跨语言 gate 或文件锁。它会扩大稳定 eSIM/BLE 硬件回归面，却不能提升生产所有权或真机业务能力；收益尚未超过成本。保留 `native_session_owner_not_ready` fail-closed 保护，待 actor 能进入 CTExcel/giffgaff 真机门禁时与 adapter 一并实施。

### 2.10（继续）：会话计划绑定 PC/SC AKA 身份

- 基线：会话计划已经把同一次读卡取得的 PLMN/GID1、运营商模板和代理决策固化，但下一阶段 IKE owner 若再次按“当前读卡器”读取 AKA，将重新引入切卡或多读卡器时的身份混配风险。
- 目标/测量：计划在读卡事务成功后立即建立不可序列化、不可调试的 `ReaderAkaBinding(reader, USIM, ICCID, IMSI)`；IKE EAP-AKA 回调只能消费该绑定。SIM 成功、同步失败和认证拒绝必须一对一映射为 Rust IKE 的 `SimAkaResult`，不记录挑战、订户标识或密钥。定向测试覆盖三种结果映射，`proletariusd` 测试与格式/无警告构建必须通过。
- 实施与验证：上下文现改名为不可 Clone/Debug/Serialize 的 `NativeSessionContext`，持有受零化保护的 AKA 绑定、已验证传输、tunnel 快照和后续 IMS 快照。`start_tunnel(&self)` 不再消耗上下文；未来唯一 supervisor 独占上下文并保证每个 generation 最多启动一次，因此后续注册续租或失败收口不会丢失原始 carrier/reader 归属。内部 plan 会在将来由已租约 actor 执行一次只读 PC/SC 身份快照和有界代理选择，绝不发 IKE 或 IMS 数据；当前公开路由已关闭，真正 `start_tunnel` 也没有 HTTP/UI 入口，因而不改变现有 Go 会话。调用方 reader/ePDG/proxy 的格式会在读卡或探测前失败，读卡忙、缺卡、超时与协议错误则保留可操作的状态码和重试语义。AKA 三分支定向测试与 daemon 定向编译测试通过。
- 成本取舍：只复用现有 `ReaderAkaBinding` 和 `SimAkaResult`，未加入新 socket、后台任务或硬件访问路径。它直接消除下一步 owner 接线中最危险的串卡入口，收益高于少量封装成本；但没有建立可验证的 IMS/SMS/语音生产会话，故不改变 `reader_vowifi` 的 Go owner，也不改变 9/15（60%）指标。

### 2.10（继续）：不可变 IMS 注册策略与安全请求编译

- 基线：Rust 已能完成 TCP REGISTER 状态机、AKA、Security-Server 解析、AES-CBC/HMAC-SHA1 ESP 安装和有序 SIP 序列化，但 carrier registry/YAML 的 `ims_*`、Contact 模板、注册计时器和设备身份语义没有进入原生会话。若直接切换 owner，CTExcel/giffgaff 可能退回通用 Contact，导致“显示已注册但无法接听”或错误的安全协商；直接复制 Go 默认六条安全机制还会让 P-CSCF 选中 Rust 无法安装的 MD5/3DES/null 套件。
- 目标/测量：同一 SIM/carrier/override 快照必须解析成 `NativeImsRegisterProfile`，并以纯函数编译 Initial、plain authenticated 和 protected 三阶段 REGISTER。CTExcel Contact 必须保留 mmtel ICSI、`+sip.instance`、mid-call/SRVCC feature tags；giffgaff 必须使用 GSMA host-only Contact 且不混入 PPI/PVNI/Cellular-Network-Info；受保护阶段必须使用 port-s、远端 port-s 和 P-CSCF 的原始 `Security-Verify` 字节。每个阶段都由现有事务 serializer 验证，禁止 CRLF 注入或重复核心事务字段。
- 实施与验证：新增 `ims_profile`，由 `ResolvedCarrier::native_ims_register_profile` 从同一读卡快照构建 domain/realm/phone-context、明确的 P-CSCF 数字 IP 覆盖、TCP、本地端口、TCP/OPTIONS 计时器和三套模板。经独立审阅后，CTExcel/giffgaff 的初始 `aka_empty` 已统一使用派生 IMPI `IMSI@realm`；`sec-agree=auto` 且 401/407 缺少 `Security-Server` 时，普通认证重试会明确删除 Security-Client/Require/Proxy-Require，而不是反复要求 P-CSCF 协商不可用的 IPSec。显式 `ims_pcscf` 必须与 runtime 实际选择完全一致；初始拒绝 fallback 和重试策略保留“缺失”与“显式空数组”的语义；非 strict Security-Server 选择在原生 secure mode 下提前拒绝，避免后期选到 Rust 无法安装的套件。`ims_identity_source=isim|auto`、UDP、无效 P-CSCF 以及非 AES-CBC/HMAC-SHA1-96/ESP/trans 的显式安全机制均在任何网络 I/O 前失败；这不是删除用户配置，而是避免 Rust 错误宣称已安装不支持的 SA。设备身份现在也成为 profile 的不可变输入：giffgaff 固定使用既有 Go 的 iOS User-Agent，其余模板遵循现有 device model；`+sip.instance` 只能由稳定的 15 位 IMEI 生成，不能由 transaction runtime 注入或退回 UUID。daemon 只读既有 Go 的 `~/Library/Application Support/Proletarius/imei.json`，将其绑定进 `NativeSessionContext`，不写入、轮换、记录或返回该值；缺失或无效身份在任何 transport I/O 前以 422 静态错误停止。plan 仅返回非订户的 identity mode/enabled 诊断。IMS 41 项、Rust VoWiFi 120 项、daemon 121 项定向回归通过；同轮 ESTKme-RED 只读探测确认 USIM/ICCID/IMSI/MNC/GID1/GID2/SPN 字段可用且不输出订户值。
- 成本取舍：没有新依赖、DNS、socket、后台任务或实际注册；初始 AKA bootstrap 维持模板位置，已计算的 Authorization 继续由既有安全状态机提供。完整 Go byte-for-byte 比较不作为门禁，因为 Go 的 From tag/Via branch 是随机运行值；改为固定合成 ID 的 ordered-header/feature 语义差分，后续再将其接入真实 supervisor。Rust 在 Go compatibility owner 仍运行时只读身份而不生成/写入，避免产生第二个 IMEI 导致 CTExcel/giffgaff 被叫绑定漂移；读取缓冲和反序列化字段在桥接范围内零化，但最终 SIP 线稿仍包含运营商所需订户身份，不能将普通临时 `String` 当作完整内存隔离。实际注册、来电、SMS/Call 业务 owner 仍未切换，故生产所有权仍为 9/15（60%）。

- 身份内存边界补充：配置原始字节和成功解码后的临时 IMEI 会在桥接范围内清零；畸形 JSON 的 Serde 内部临时分配不能被宣称为完整零化保证。公开 API、日志和诊断仍不返回订户值，最终 SIP 线稿也必然携带运营商所需身份。若要把畸形输入路径也纳入强零化保证，必须引入专用 guarded visitor，而不是扩大为全局 `zeroize` 的 Serde API。

### 2.10（继续）：按读卡器/SIM 隔离的原生会话所有权核心

- 基线：原生 IKE、ESP、TCP、REGISTER 和 AKA 已各有可测试组件，但没有按读卡器与卡身份隔离的 daemon owner。若直接在 UI 或全局 active-session 上接线，多读卡器、切卡、旧任务延迟结束都会导致串卡、旧 generation 清掉新状态或错误的“在线”展示。
- 目标/测量：建立无网络副作用的 `NativeVoWiFiSupervisor`：每个私有 `ReaderSessionKey(reader, sim)` 只允许一个 opaque generation；Prepared、Running、Stopping 转换必须精确匹配该 generation；stop 只通知所属 worker，worker completion 只删除所属 generation。读卡器名称或 SIM 标识不进入日志、JSON、Debug 或公共路由。
- 实施与验证：`NativeSessionContext` 现从同一次 PC/SC ICCID 快照派生并持有该 key，不能接受 HTTP 传入的 SIM key。supervisor 不启动 IKE、不读卡、不做 DNS/代理探测、不 spawn task；它只提供 reservation、start、generation-scoped stop、all-stop 与 worker-finished 归约。定向测试覆盖键值校验、双设备隔离、重复 start、准备期 stop、idempotent all-stop，以及迟到旧 generation 完成无法删除 replacement。daemon 的 VoWiFi 相关单元与 strict Clippy 通过。
- 成本取舍：此轮刻意不把 dormant reducer 宣称为 live session actor，也不改变 UI 路由或 Go owner；收益是先固定最关键的多实例/回收不变量，成本仅为一个小型无 I/O 核心。未来真实 actor 必须固定执行 `停止新命令 → RegisteredImsSession.shutdown() → IKE stop → 等待同 generation worker → worker_finished`；因为 SecurityLease 清理依赖仍在运行的 packet owner，绝不能反过来先停止隧道。未完成 CTExcel/giffgaff 的真实 IKE→IMS→SMS→主被叫门禁前，生产所有权仍为 9/15（60%）。

### 2.10（继续）：建立后 IKE 主动关闭的有界优先级

- 基线：空闲测试已验证本地主动 IKE DELETE，但普通 IP/DPD 控制和 DELETE 共用 FIFO，且停止后上层不再消费 event 队列；在持续 ESP/控制流量下，积压或 event 回压可能让 data plane 先失败，导致资源虽释放却没有发出 DELETE。
- 实施与验证：本地 DELETE 改走独立 one-shot 高优先级通道；data plane 在常规循环、普通命令发送、NAT keepalive 发送和 event 回压等待时都优先消费它。event 满时仅保留 250 ms 可抢占等待，未收到 shutdown 才保持既有 fail-closed 错误。动态测试覆盖 DPD 队列积压加 event backlog、以及真实阻塞在 event 发送时的 stop；全量 IMS 41、VoWiFi 120、daemon 121 测试、strict Clippy、release 构建与 diff check 均通过。
- 成本取舍：不新建 socket、不改前置代理路径，也不把 tunnel 成功上报为 IMS/SMS/通话可用。双方在极窄窗口同时发 DELETE 的 RFC SHOULD 宽限仍未引入额外关闭后接收状态机；收益不足以覆盖新增复杂度。真实 session owner 和运营商真机门禁未完成，生产所有权仍为 9/15（60%）。

- 复测记录：后续 fail-closed owner gate 与设备身份零化调整已通过同一全量回归；当前计数为 IMS 41、Rust VoWiFi 120、daemon 122，且 `cargo fmt --check`、strict Clippy、release 构建与 diff check 均通过。该数字仅是测试覆盖，不改变生产所有权 9/15（60%）。

### 2.10（继续）：传真取消确认与单次归档

- 基线：本地取消请求曾在 NAS Agent 的 HTTP 请求失败、超时或返回未识别状态时直接把会话归档为已取消。这会造成界面显示已停止、而 NAS 任务仍在拨号或发送的假终态，也会让轮询与取消竞争时产生重复历史记录。
- 目标/测量：仅当 Agent 返回明确的 `cancelled` / `canceled` 才将本地会话置为失败终态并写入一次归档；`completed` 与 `failed` 只按其真实终态归档；仍活动、未知、超时和传输错误必须恢复可轮询状态、返回可重试的 `fax_cancel_unconfirmed`，且历史条目数保持为零。并发的轮询、取消与超时最多写入一条记录。
- 实施与验证：`cancel_fax_job` 现解析 Agent 的 `FaxJob` 状态；`Cancelling` 维持为非终态，未确认时恢复先前或轮询观察到的活动状态。`finish_fax_poll` 在会话锁内先提交唯一终态，再在锁外归档。16 项定向传真测试覆盖确认取消、未确认恢复、并发终态 exactly-once、编码和收件契约；`proletariusd` strict Clippy 通过。
- 成本取舍：不更改 NAS SSH、103 分机、T.38/PCMA、Swift API 或真实电话/传真会话。Agent 仍是远端任务状态的权威来源；本机没有把“HTTP 成功”伪装成取消成功。传真已是 Rust 生产组件，本修复提升其失效语义但不增加 Rust/Go 所有权计数。

### 2.10（继续）：ESTKme-RED BLE CCID 精确通道与可选续协商

- 基线：CCID 发现允许任意可写/可通知特征值作为兜底，且飞行中的 APDU 可以接收非所选特征值的通知；具有多个服务的蓝牙外设可能将无关数据误当成 CCID 回复，表现为已发现却无法稳定连接。首次 claim 后的上游可选 `61 <len>` 续协商也未被处理。
- 目标/测量：仅对 CCID 型 ESTKme-RED 绑定 `4553` service 内具备正确属性的 `544B` notify/indicate 与 `6D65` write；通知状态和数据回调都必须来自该同一对象。claim 回复要求时发送 `FF FF FF 01 <len>` 控制帧，再执行上电；续协商本身失败时按上游兼容策略回退到既有上电流程。legacy 与 BeeSIM 的发现路径不得变化。
- 实施与验证：`BluetoothReaderManager` 已移除 CCID 的泛化 fallback，并将 APDU 收包与通知启用限制为选择的特征对象。macOS Debug `xcodebuild`（不签名）通过；没有扫描、连接读卡器、读卡、切卡或启动网络会话。
- 成本取舍：不扩大扫描范围、不调高超时、不加入重试循环，也不假称 ESTKme-RED 2.0.3 已完成真机验收。该补丁降低既有 Swift BLE bridge 的错误通道风险；它不改变 Go 硬件 owner 或生产所有权 9/15（60%）。

### 2.10（继续）：读卡器/SIM 代际快照防串卡

- 基线：一次设备快照会并发读取 Go 的 `reader/info` 与 `reader/vowifi`，原校验只比较 selected reader 名称。切卡、重连或旧会话收尾的窗口中，新卡身份可以与旧卡的 IMS/SMS/通话 ready 状态被拼接；VoWiFi 启动本身也曾先读取 SIM/generation、再独立读取 reader 名称，可能构造跨代 session key。
- 目标/测量：每个可用的 VoWiFi 状态必须同时匹配当前 `reader_id + reader_generation + ICCID`（ICCID 缺失时才使用 IMSI）；任一项缺失或不一致时，快照不得保留 `vowifi` 负载，必须产生可重试 issue。普通 idle/disconnected 状态无需伪造 session identity。GET 状态接口不得以修复为由停止会话、连接设备或修改 Profile。
- 实施与验证：Go 以同一锁下的 reader name、generation 和缓存 SIM 生成/校验 session key，并在状态中返回 generation；不匹配的旧 session 被降为 unavailable/disconnected。Rust 合约显式解码三个绑定字段，snapshot 对具会话含义的状态做 fail-closed 隔离。Go 定向状态测试、Rust API contract 5 项与 snapshot 6 项、相关 strict Clippy、格式/diff 检查通过；macOS Debug App 构建成功。
- 成本取舍：代际快照路径本身未新增 UI、读卡、代理或网络操作；热拔清理改由下述独立、受限的 monitor 承担，不在 GET 快照中运行。该修复只保证 UI 不会重发布无法归属的绿色状态；DJI/eSIM、BLE inventory 与实际 reader VoWiFi session 仍依赖 Go，因此生产所有权保持 9/15（60%）。

### 2.10（继续）：快照只读与两次确认热拔监视

- 基线：`/api/v2/device-snapshot` 高频读取时若把一次不完整的 USB/蓝牙发现结果直接当作物理拔卡，会在页面刷新期间断开读卡器或停止正在进行的 VoWiFi；反之，单纯保持只读又会使真正拔卡后的 Go 会话没有独立的回收入口。
- 目标/测量：快照兼容接口只能观察 presence/identity，绝不连接、断开、停止 VoWiFi 或修改 Profile。独立 monitor 只在存在已选读卡器时运行，按该读卡器所属传输执行有 1 秒截止时间的扫描；连续两次确认缺失后才清理对应 generation。扫描、桥接或忙错误不得计为拔卡，后续重新发现必须清除缺失计数。
- 实施与验证：私有 `readerReconcile` 保持 `observePresence`，Rust 快照继续对 `reader_id + reader_generation + SIM` 不一致 fail-closed。Go 的独立 hot-unplug monitor 每 2 秒串行轮询一次，在两次缺失后先断开该 reader，再以捕获的 generation 调用 `stopVoWiFiForReaderGeneration`；迟到的旧监视结果不能停止新会话。定向 Go 测试覆盖两次去抖、重新发现、扫描错误、无选中读卡器、操作锁、串行轮询、截止时间与 context 结束；既有 Rust snapshot/inventory 门禁继续覆盖只读聚合路径。未在本轮执行物理插拔或运营商真机循环。
- 成本取舍：新增的是窄范围、只在已选读卡器存在时激活的硬件生命周期 monitor，而非页面轮询或网络保活；用两次独立确认换取避免瞬时扫描失败中断通话。它仍调用 Go 的设备/会话清理，`device_snapshot` 仍为 aggregate-shadow，`reader_vowifi` 仍为 Go production owner，因此 Rust 生产所有权保持 9/15（60%）。

### 2.10（继续）：生产 Go 会话精确采用可表达的运营商 IKE/ESP 模板

- 基线：内嵌的 `ios-carrier-profiles` 快照已能选择 CTExcel 与 giffgaff 模板，但 Go 生产 `swu-go` session 只消费 `effective.IKEProposals` / `ESPProposals`。用户没有写 YAML 覆盖时，这两个列表通常为空，运行时会回退到泛化候选集，而不是采用模板中已经可表达的首选 CBC/SHA-2/DH14 组合。
- 目标/测量：无用户 proposal、algorithm policy 或 legacy 请求时，只让单一、完整且当前实现可表达的 IKE/ESP 模板进入 production session；CTExcel 必须得到 `aes128-sha256-prfsha256-modp2048` / `aes128-sha256`，giffgaff 必须得到 `aes256-sha512-prfsha512-modp2048` / `aes256-sha512`。显式列表、policy/legacy 始终优先；DH2、多个 transform、AEAD、未实现散列或必需 ESN 不得被伪造为 DH14 或静默降级。
- 实施与验证：carrier 展开层仅记录不可序列化的“单一提议可精确表达”元数据，并校验 `child_sa.esn` 只有空值、`auto` 或 `noesn` 才兼容当前 NO_ESN runtime。`carrierTemplateProposals` 由 production effective-config 在已有覆盖优先级之后调用。定向测试覆盖 CTExcel、giffgaff、显式 proposal/policy 优先、DH2 不映射与必需 ESN 拒绝；carrier 定向测试和 reader 目标测试通过。
- 成本取舍：不新增模板、不改 selector、上游数据或网络行为；不把未来 ESN/DH 能力假称为已支持。此修复只缩小当前 Go session 的协商漂移，仍需 CTExcel/giffgaff 真机 IKE→IMS→SMS→双向通话验证，Rust `reader_vowifi` owner 因而仍为 Go，生产所有权保持 9/15（60%）。

### 2.10（取舍）：Rust 原生会话保持结构化 fail-closed

- 基线：Rust 已具备原生 tunnel、SIM/运营商快照和会话准备代码，但没有跨进程读卡器 lease、按 reader/SIM/generation 管理的 live actor、受保护 IMS runtime 或实时 SMS/Call runtime。直接打开会让 Rust 与当前 Go session 同时访问 USB/BLE 读卡器，存在串卡与假在线风险。
- 实施与验证：`native-session-plan` 在读取 body、触卡、探测代理或建立网络前固定返回 HTTP 409，并列出 `cross_process_reader_lease`、`keyed_lifecycle_actor`、`protected_ims_runtime`、`live_sms_call_runtime` 四项缺口；没有环境变量绕过。定向 daemon 测试验证四项均为门槛、响应契约和 fail-closed 顺序。
- 成本取舍：本轮不提前实现长期硬件 lease 或新增公共 API，因为没有 live actor 消费它，收益不足以抵消 eSIM/BLE 回归面。待 Go 私有、generation-bound lease 与真实 actor 同时具备，并通过 USB/BLE、CTExcel/giffgaff 真机门禁后，才允许改变该门槛或提升所有权读数。

### 2.10（继续）：长连接真实健康与切卡通话状态隔离

- 基线：IMS OPTIONS 先前把任意最终 SIP 响应视为心跳成功；403/5xx 等已拒绝的业务路径仍可让 `MessagingReady`、SMS/Call ready 及 UI 停留为绿色。纯 Go 状态轮询在连续失败时也保留最后一个 ready 负载。独立的 `/api/reader/vowifi/calls/status` 还只读取全局 session，外部切卡后可将旧 generation 的来电或通话状态显示给新 Profile。
- 实施：OPTIONS 仅以最终 2xx 维持健康；provisional 继续等待，非 2xx、超时、流关闭都进入已有 runtime failure/自动重连链。401/407 暂按绑定/认证失效 fail-closed：当前受保护 OPTIONS 只有 `Security-Verify`，其 AKA/Digest 重挑战只在 REGISTER 流程实现，不能把认证挑战伪装成健康。Swift 仅容忍一次状态读取失败，连续两次才撤销旧 VoWiFi 状态，且不影响正在启停的操作。所有 reader 通话状态及拨号、接听、拒接、挂断、DTMF 均复用 reader/SIM/generation guard；失配 status 返回 unavailable，控制操作返回既有 409。
- 验证：定向 Go 测试覆盖 `100→200`、仅 provisional、`100→403`、关闭响应流，以及旧 generation 的 status 和 dial 拒绝；Swift 通过 macOS 27 编译门禁。没有发起运营商、代理或读卡器真机操作。
- 成本取舍：不增加通用 OPTIONS Digest retry，以免在未验证的 AKA/IPsec 重同步中制造新的注册循环；未来只有在特定运营商真机证明确实要求 authenticated OPTIONS、且其后 SMS/Call 正常时，才增加 carrier-gated 重挑战。此轮同步降低假在线与切卡串卡风险，但不能代替 CTExcel 被叫 INVITE 抵达 port-s 的真实 trace，因此生产所有权仍为 9/15（60%）。

### 2.09：Surge 自动代理识别与完整 REGISTER 动态门禁

- 基线：macOS System Proxy 下 Surge 只向 System Configuration 发布 HTTP `127.0.0.1:6152`，实际 SOCKS5 监听位于 `127.0.0.1:6153`；旧自动模式只读取 `SOCKSEnable`，因而在 Surge System Proxy/Enhanced Mode 已启用时仍可能把代理留空，并在 fake-IP 场景被 Rust 传输策略正确拒绝。旧的 netstack cooperative-yield 测试还依赖 Tokio 一次 `yield_now` 后的调度顺序，不能确定地证明停止信号在持续 ready 输入下会被处理。
- 目标/测量：仅当系统 HTTP/HTTPS 代理为 loopback、Surge 报告 `system-proxy: On`，且系统端口与 Surge 有效配置的 `http-listen` 完全匹配时，才采用同一有效配置中的本地 `socks5-listen`；不能猜测端口、把普通 HTTP 代理当 SOCKS，或影响 Clash/远程代理/合法直连。实机测量使用当前 Surge 6.10，要求自动发现的 SOCKS 端点完成真实握手，并由 Rust 在 SOCKS UDP ASSOCIATE 不可用时选择已经验证的 Enhanced Mode/fake-IP 路由，绝不回落公网 fake-IP 直连。
- 实施：`systemSocksProxy` 保留显式 macOS SOCKS 最高优先级；Surge 分支以 3 秒、2 MiB 上限读取有效配置，交叉核验 `http-listen` 与 System Configuration，再返回 loopback `socks5-listen`。当前实机结果为 SOCKS 握手成功、UDP ASSOCIATE 不可用、Enhanced Mode 路由验证成功，最终选择非直连透明传输，符合 Surge 与 Clash 能力差异。确定性 netstack 门禁改为预填 `WORK_BEFORE_YIELD + 1` 个持续 ready 报文，在 `unconstrained + join!(biased)` 的第一个真实 Pending 边界发送 stop，并精确断言只处理 32 项。
- 完整注册门禁：双 `TunnelNetwork` 与双 `SecurityNetworkRuntime` 使用真实镜像 ESP 策略，贯通 plain REGISTER → 401/AKA → 原子 sec-agree 安装 → port-s ready → protected port-c REGISTER → 同一 write 中的 200+INVITE → 原 Primary 连接 180；shutdown 后双方先清旧 SA 再安装新 generation，并验证 5063 listener 与 5064 四元组立即复用。该测试同时证明 replay window 正确拒绝“相同 SPI/key 但序列重置”的错误测试复用。
- 验证：当前 Surge 实例的自动发现、真实 SOCKS 握手与 Rust 非直连传输选择 5 项动态检查通过；Rust VoWiFi 90 项、Rust 网关 102 项、Go vet/tests、PCMA/IMS SDP/AMR 差分和传真契约门禁全部通过，workspace strict Clippy 零警告。2.09 macOS 27 arm64 Release App 已完成版本、架构、最低系统和深层签名校验；本阶段按约定不生成 DMG。
- 成本取舍：不引入后台代理扫描、不读取或记录节点内容、不根据进程名猜测端口；Surge CLI 只在 loopback System Proxy 候选下执行。SOCKS UDP 不是 Surge Enhanced Mode 的必要条件，因此门禁记录能力而不强制伪造支持。下一硬前置是 CTExcel/giffgaff 实际模板要求的 IKE/ESP AES-CBC + HMAC-SHA256/SHA512；在该数据面和 daemon session owner 完成前，生产 reader VoWiFi 仍由 Go 持有，所有权保持 9/15（60%）。

### 2.09（继续）：CTExcel/giffgaff CBC/HMAC 原生链路

- 基线：Rust 原生候选虽已能校验 CBC SA、派生完整性密钥，但 proposal、IKE SK、全部 IKE_AUTH/DPD/DELETE 分派及 CHILD ESP 数据面仍只允许 GCM。实际模板中 CTExcel 使用 `aes128-sha256-prfsha256-modp2048` / `aes128-sha256`，giffgaff 使用 `aes256-sha512-prfsha512-modp2048` / `aes256-sha512`；683 个模板中 256 个明确发布 CBC，GCM-only 原语不能承载两张目标卡。生产 Go 空 proposal 还会展开为宽泛兼容列表，不能原样迁移到 Rust 的 fail-closed 空列表语义。
- 目标/测量：只新增 AES-CBC-128/256、HMAC-SHA2-256-128/512-256，保持 AES-192、SHA1、SHA384、ESN 和初始 Child PFS 拒绝；IKE ICV 覆盖 `IKE header..ciphertext`，ESP ICV 覆盖 `SPI|Seq|IV|ciphertext`，接收必须先验 ICV、再解密，replay 只在认证及结构检查后提交。模板必须生成精确非空单运营商 proposal；CTExcel/giffgaff wire、key length、双方向 SK/ESP、篡改、坏 padding、重放和计数耗尽均纳入动态测试。
- 实施：IKE crate 新增 CBC SK 与 CBC ESP sender/receiver，CSPRNG 生成 16 字节 IV，敏感 key/plain 使用清零容器，截断 HMAC 固定时比较。`SaInitKeySchedule` 和 `IkeAuthSession` 保留 integrity transform；EAP、最终 AUTH、DPD、peer liveness 与 DELETE 统一走 suite dispatcher。CHILD SA 由协商结果构造 GCM 或 CBC 数据面；原 GCM-only API 仍保留为兼容门禁。proposal builder 新增受限的 native 入口，旧 GCM 入口不会静默扩权。Rust carrier resolver 从已经完成 selector 合并的 Apple 模板提取精确 proposal，遇到多个无法无损表达的 transform 或未实现算法立即拒绝。
- 验证：Rust IKE crate 38 项、VoWiFi 集成 92 项全部通过；其中两套运营商 proposal 与 Go wire 完全一致，双方向 IKE SK、IPv4/IPv6 ESP、错误 key/ICV/padding/replay/sequence fault injection 均通过。Go `swu-go` crypto/proposal/ESP/SWU 回归同步通过，Rust 两 crate strict Clippy 零警告。
- 边界：本轮完成原生协议链本身，不代表生产 API 已切到 Rust。Go 仍持有 reader VoWiFi 真机会话；SKF fragmentation、Child rekey/PFS、daemon 原子 session owner、Go/Rust 动态互解 fixture，以及 CTExcel/giffgaff 各三轮 IMS/SMS/双向通话真机验收仍是切换门禁。因此所有权保持 9/15（60%），不删除 Go 路径、不生成 DMG。

### 2.08：取消安全 SIP 写入、精确关闭与 MT 连接隔离

- 基线：注册协调器已能建立受保护连接和接收 MT 信令，但 `write_all` future 若在部分写入后被取消，下一次发送可能把新 SIP 帧接在半帧后；listener accept 与 TCP close waiter 被取消后也必须能恢复等待，不能遗失 socket owner 或提前复用端口。
- 实施：每条 primary/incoming 连接独立持有 `Open / Closing / Closed` 状态以及待发送帧、已写 offset、flush 阶段和绝对 deadline。取消后先续完旧帧才接受新帧；stop、deadline 或 I/O 错误进入 move-only `PendingTcpClose`，精确 close ack 完成后才允许清 SA。`PendingTcpListener::accept_cancel_safe` 和 close token 均可在 `select!` future 被丢弃后继续轮询。
- 测量：动态双 netstack 测试覆盖半写取消续传、incoming 超时只关闭目标连接、primary 继续可用、accept 取消恢复、close 取消恢复和同端口立即复用；IMS 40 项、VoWiFi 89 项、workspace tests 与 strict Clippy 全部通过。用户明确要求后生成并校验 2.08 macOS 27 arm64 DMG；这不改变生产 owner，Go 后端继续保留。

### 2.07：原子 sec-agree、注册协调器与 MT 信令连接

- 基线：2.06 具备动态 TCP 与安全关联底座，但 `ReadySecAgree` 的策略和注册 continuation 曾可在公开边界拆开，调用方理论上能把相同 selector、不同 AKA 会话的安装凭证交叉组合；REGISTER 的 plain/protected 两段仍由调用方分散编排，port-s listener 虽已创建却没有 accept/回包 owner，无法形成真正的 MT 来电链路。
- 目标/测量：把一条 AKA 会话的策略、continuation、安装 generation 绑定为不可拆分的 move-only 证明；一个 coordinator 在同一绝对 deadline 与 stop 信号下执行 plain REGISTER、AUTS、sec-agree 安装、port-s 监听、protected port-c REGISTER 和失败回滚；来电必须从 port-s 被上报并在原连接回包。门禁覆盖跨会话误配、安装回复取消、未 service runtime 的 stop/deadline、listener accept 取消恢复、来电 INVITE 同连接 180 回包和立即重新监听。
- 实施：`SecuritySlot::install_registration` 在 packet owner 内原子拆包、校验 binding 并安装，外部只得到 opaque installed proof；进程内唯一 binding ID 阻止即使 selector/密钥相同的会话互换。`SecurityControl` 将整个 `ReadySecAgree` 送入网络 owner，取消 reply waiter 会由融合 proof/lease 在下一 packet 边界前按 generation 清理。`registration_coordinator` 统一两个 TCP 阶段与显式 socket-before-SA 回滚，不把命令已入队误当成连接成功。
- MT 来电：`PendingTcpListener` 支持可在 `tokio::select!` 中重复轮询的取消安全 accept；注册会话同时拥有 port-c 主连接、port-s listener 和有界的入站信令连接。事件携带连接 ID，100/180/200 等响应必须写回同一入站连接；accept 后立即重臂 listener，停止时先关闭所有 TCP owner，再撤销精确 SA generation。
- 成本取舍：复用现有 smoltcp registry、SIP framer 与安全 slot，无新依赖、无逐包锁、无分离后台 reader。入站信令连接固定上限为 2，超过上限时不继续扩容 socket；该上限先用动态测试验证，真机数据证明不足后再调整。
- 验证边界：本阶段的本地双 netstack 测试证明注册资源顺序、取消与 MT 信令路由，不等同运营商 CTExcel/giffgaff 真机注册、SMS、PCMA 已由 Rust 生产接管。生产通信所有权仍 9/15（约 60%）；保留 Go 后端，不删除生产路径，本阶段不生成 DMG。

### 2.06：有界安全墓碑与动态多 TCP/监听底座

- 基线：2.05 已能在运行中的单 packet owner 内安装 IMS 安全关联，但 socket 仍必须在网络启动前静态创建，无法按 `install ack → port-s listener ready → protected port-c connect` 的真实顺序继续注册；撤销 SA 后保存的精确 selector 墓碑也会随长期重认证无限增长。目标是先完成可测量的网络底座，不提前修改 REGISTER 状态机或解除生产防护门禁。
- TCP registry：新增单网络任务持有的有界 `TcpControl`/`TcpSocketRegistry`。动态 `dial` 与 one-shot `listen` 的命令确认分别表示 socket 已创建及监听已就绪，TCP `connect`/`accept` 单独等待；move-only pending/managed stream 通过与 entry 直接绑定的 close/closed oneshot 完成精确回收，不按可复用 handle 路由清理。取消等待者、迟到关闭、EOF 或 RST 只回收对应 socket，不终止其他连接或整个 tunnel，也不会因应用暂持已失效 stream 占满 cleanup permit。重复监听端口及重复外拨四元组在创建前拒绝。listener 可长期等待来电，不套用 30 秒外拨握手超时；本阶段接受一个连接后由下一阶段注册协调器先重臂 listener，再分发该连接。
- 组合顺序：TCP registry 与 `SecurityNetworkRuntime` 共用 wake channel，并作为其 application service 在同一任务、同一 `SocketSet` 中执行，不引入逐包锁或跨进程 IPC。动态测试先从普通端口生成 protocol 6 SYN，再等待安全安装 ack、等待本地 port-s 监听 ack，最后才从 port-c 发起连接；对端密钥实际解密首个 protocol 50 报文并断言内层为 `5064→6060` TCP SYN，任何同源端口的明文 TCP 都使测试失败。teardown 先等待 port-c/port-s `close()` ack，再撤销安全 lease，形成可证明的 socket-before-SA 屏障。
- 长期边界：精确 retired selector 最多 16 项；达到阈值后按本机/P-CSCF 地址对合并为最多 8 个 fail-closed 域墓碑，再超限则阻断所有非 active TCP/ESP，而不是淘汰旧记录并回退明文。100 次重认证验证集合有界、旧流及同域其他 TCP 均被拒绝，当前 active selector 仍优先正常保护。
- 测量：覆盖命令入队但 ack 前取消、100 次 dial waiter 取消无容量泄漏、重复端点拒绝、拒绝连接不影响后续成功连接、读写往返、复用 handle 后旧 entry 的迟到 close 不影响新 socket、可等待 close 屏障、动态 port-s-before-port-c 排序，以及安全墓碑 100 次同域轮换和 `16 exact + 8 domains → global` 上限。运行 IMS/VoWiFi 定向测试、workspace 测试、Clippy `-D warnings` 与 Release 构建。
- 成本取舍：动态 registry 复用既有 smoltcp/duplex 驱动且保持 16 socket 硬上限，收益高于维护第二套模拟 coordinator；暂不做常驻多 accept 队列或多 SA 热切换。`RegistrationConnection::SecurityTransportUnavailable` 继续保留，生产 owner 不切换；下一阶段才实现可跨连接的 type-state REGISTER continuation、AUTS 原连接路径、listener 重臂及 MT INVITE 分发。
- 验证边界：本阶段证明 socket 生命周期和 sec-agree 排序底座，不证明运营商 REGISTER、来电、SMS 或 PCMA 已由 Rust 接管。生产所有权仍为 9/15（约 60%），不删除 Go 业务，不生成 DMG。

### 2.05：sec-agree 原样协商与单 owner 动态安装门禁

- 基线：2.04 的 `SecurityTransport` 只能在测试启动前固定安装，`NativeTunnel::run_network` 生产入口仍是 identity 传输；Rust 解析器也把 O2/giffgaff 六项 `Security-Server` 中省略 `ealg` 的合法 null-encryption 提议误判为不完整。目标是先建立真实运行中可安装的单 packet owner 边界，且不提前解除 `RegistrationConnection` 的明文防护门禁。
- 协商：省略 `ealg` 规范化为 `null`，按算法、加密、协议和模式选择首个最高 q 的完整提议；同一列表中的 TLS 等未实现机制不参与选择但保留在回显中。`NegotiatedSecurity` 同时保留 P-CSCF 原始 `Security-Server` 文本作为 `Security-Verify`，不重排提议、不展开只位于末项的共享 SPI/端口。完整六项 O2/giffgaff 样本选择 SHA1/AES-CBC q=0.98，并与 Go 生产行为差分。
- 生命周期：新增单任务拥有的 `SecuritySlot`。安装产生不可 Clone/Debug 的 receipt 和单调 generation；occupied 时拒绝第二 SA，`clear_if` 仅撤销精确 generation，迟到清理不能移除新 SA。撤销后的精确旧 selector 保留 tombstone，在整个 tunnel runtime 退出前不得降级成明文。
- 运行时：`TunnelNetwork::run_managed` 让同一对象同时处理 socket 与 packet transform；`SecurityNetworkRuntime` 通过有界命令队列在网络任务内安装/撤销，不增加逐包 mutex 或跨进程 packet IPC。安装 ack 是打开受保护 port-c 连接前的排序屏障；若等待者在 ack 前取消，刚安装的 SA 立即回滚。`NativeTunnel::run_network_managed` 与 IKE/ESP future 共生命周期，既有 identity 入口保持兼容。
- 动态测量：真实 smoltcp 先从初始端口产生明文 TCP SYN，运行中提交策略并等待 ack，再从协商端口产生的首包必须为 protocol 50；若出现该端口明文 TCP 测试立即失败。另覆盖取消 ack 自动回滚、occupied 拒绝、迟到 generation 清理不影响新 SA、clear 后旧流量丢弃和无关流量透传。
- 成本取舍：否决每包 `Arc<Mutex>` 的低改动方案，因为热路径锁和第二控制 owner 的长期成本超过收益；暂不实现 active/pending 多 SA 热切换，续期仍采用停旧会话并 join 后全链路新建。下一阶段必须完成统一多 TCP driver、受保护 port-c 和 final REGISTER 前 port-s listener，再将 receipt 接入 type-state REGISTER；在此之前保留 `SecurityTransportUnavailable` 门禁和 Go 生产 owner。
- 验证边界：本阶段证明动态安装顺序和失败闭合，不证明真实运营商 REGISTER、来电、SMS 或 PCMA 已可切换生产。生产所有权仍为 9/15（约 60%），不删除 Go 业务，不生成 DMG。

### 2.04：坏包隔离、三态路由与 ESP 线长观测

- 基线：2.03 中任一入站转换错误都会结束整个 smoltcp 网络循环，单个坏 ESP/IP 包可打断 IMS 长连接；保护流量与非 IMS 流量也缺少显式的保护、透传、丢弃三态边界。
- 实施：IMS 安全传输仅保护精确匹配的 TCP 双向端口；非目标流量显式透传，目标流量的明文入站显式丢弃。畸形包、错误 SPI、坏 ICV 与重放只计数丢弃，熵源失败和序列耗尽仍为致命错误。出站转换错误或 `Discard` 保持 fail-closed，绝不以明文或静默丢包继续。
- 稳定性：网络循环返回入站转发/丢弃、出站转发及双向最大线长统计；连续即时工作每 32 次主动让出执行权，避免持续收包或 wake 事件饿死停止、看门狗和同会话任务。故障注入先送入畸形包，再完成真实 smoltcp TCP 握手和 5000 字节 SIP 回显，验证坏包不终止会话；另测出站丢弃在隧道发送前失败。
- MTU：CBC ESP 对 TCP 段长度 `T` 的载荷线长为 `36 + 16 × ceil((T + 2) / 16)`。当前 1280 内层 MTU 的 IPv4/IPv6 最大保护包分别为 1320/1324 字节；结合现有外层路径预算暂无下调证据，因此保留 1280，并以运行时最大线长为后续 PMTU/真机决策依据，不盲目降低 MSS。
- 验证边界：IMS 32 项、VoWiFi 72 项及定向网络故障测试通过后，再执行 workspace、Go 差分和 Release 构建门禁。此改动不等于安全关联已按协商动态安装，也没有切换生产 owner；生产所有权仍 9/15（约 60%），不生成 DMG。

### 2.03：IMS ESP 接入用户态 TCP 运行循环

- 基线：独立 IP 转换器尚未用于 netstack。目标：同一个网络循环在 smoltcp 与隧道队列间完成转换，拥有转换器与 socket 的共同生命周期，不新建分离后台任务。
- `TunnelNetwork::run_transformed` 提供方向明确的拥有型报文回调；原 `run` 委托零拷贝 identity 回调保持既有行为。转换失败显式退出，不退回明文。IMS 仍作为测试依赖，避免网络基础层反向绑定 IMS 协议实现。
- 测量：真实 smoltcp TCP SYN/ACK 与 5000 字节 SIP 正文分段往返经 CBC ESP 恢复一致，入站队列确认 protocol=50；原普通 TCP/RST 路径回归。IMS/VoWiFi 全量相关测试与 Rust Release 构建。
- 收益/成本：复用单运行循环而非复制网络栈，新增泛型回调无生产默认开销。未证明运营商可用；待补协商后安装/撤销、ESP 开销 MTU 预算、无效包丢弃与限流（当前转换错误结束循环），然后接入真实隧道。
- 生产所有权仍 9/15（约 60%），不修改当前连接，不生成 DMG。

### 2.02：IMS IPv4/IPv6 TCP↔ESP 转换

- 基线：2.01 仅编解码 ESP 负载。目标：将协商的双向通道作用到完整 IP 包，并保证 TCP 字节不变、IPv4 长度/校验和与 IPv6 长度正确重写。
- 新增 `security_transport`，消费安全策略构造两个独立收发通道；按本机/P-CSCF 地址、SPI 和端口精确匹配，检查 TCP data offset。非目标流量返回错误，由外层路由明确决定去向，不静默明文旁路。
- 测量：IPv4、IPv6 双端请求/响应往返字节一致，重复和错误方向报文拒绝；IMS/VoWiFi 回归与 Rust Release 构建。
- 取舍：复用 CBC ESP 和安全策略，无新依赖；暂拒绝 IPv4 分片与 IPv6 扩展头，不隐式兼容未经验证的处理。TCP 校验和仍由现有 netstack 检查，转换不改变 pseudo-header 地址或 TCP 字节。
- 仍需将转换器接入用户态 netstack、安装生命周期及真实注册；生产所有权仍 9/15（约 60%），本轮不改 UI、不生成 DMG。

### 2.01：IMS CBC ESP 基础收发

- 基线：已有 IMS 策略与密钥映射，但 Rust 现有 ESP 仅适用于 ePDG GCM tunnel。目标：实现独立 transport ESP 的 AES-CBC-128/HMAC-SHA1-96、随机 IV、32 位序列和 64 包重放窗口，后续用于 IMS TCP。
- 测量：固定密钥/IV 报文与现有 Go 实现逐字节一致；篡改不能推进接收窗口，乱序允许、重复拒绝、序列耗尽拒绝。Go ipsec3gpp、IMS/VoWiFi 回归及 Rust Release 构建验证。
- 成本：复用现有 AES/HMAC/SHA1/熵源，引入 CBC 模式库及 block-padding 两个包，而非自写分组链接算法；补齐许可证。参考 https://docs.rs/cbc/0.2.1/cbc/ ，严格先认证再解密。
- 限制：只有 ESP 负载编解码，未做 IP/TCP 头转换、端口检查和安全关联安装；不据此解开 sec-agree 门禁，不改变生产通信。生产所有权仍 9/15（约 60%），无 DMG。

### 2.00：IMS 安全通道映射与版本进位修复

- 基线：已有 Security-Server 提议和 AKA 密钥，没有双方向策略对象。目标：与 Go 对齐客户端/服务端流的 SPI、端口及 AES-CBC/HMAC-SHA1 密钥映射，为实际 ESP 收发提供明确输入。
- 新增 `security_policy`，AES-128 CK 和补零至 20 字节的 IK 使用 Zeroizing；校验地址族、非零端点及本端双 SPI 不重复。算法仅接受已明确准备支持的 AES-CBC/HMAC-SHA1-96，其他算法返回不支持，不擅自降级。
- 测量：Go/Rust 共用样本验证双流方向；Rust 验证密钥布局和无效输入，Go ipsec3gpp 回归通过。无新依赖，必要的策略层成本低于未来多处重复映射成本。
- 构建中发现两位显示版本 2.00 被原脚本错误映射为 Cargo 2.00.0；已分离显示版本与 SemVer（2.0.0），并修复默认 99→下一主版本 00 的进位。继续使用构建号 200。
- 该策略不是已安装 SA，ESP 包变换、算法选择与生产注册接管仍未完成；生产通信所有权 9/15（约 60%），不生成 DMG。

### 1.99：IMS Security-Server 参数解析

- 基线：原生注册只检测 Security-Server 的存在，尚无可供安装器使用的提议参数。目标：提供算法、加密、模式、协议、优先级、双 SPI/端口的有限解析，拒绝歧义及数值越界。
- 新增 `security_offer`，采用整数千分位优先级，不接受 NaN；头长/提议数分别限制 8192/32。支持当前 Go 样本的 `mod=trans` 和末项共享端点，填充缺失值但不覆盖显式端点；这是对 Go 无条件覆盖共享参数的有意收紧。
- 测量：现有 Go 运营商样本字段、共享端点、显式端点保留、重复字段、越界端口、无效机制；运行 IMS/VoWiFi 回归及 Rust Release 构建。独立有限解析模块无新增依赖，服务于后续真实安装器，满足成本阈值。
- 边界：尚无算法选择、完整 Security-Verify echo 和 IMS ESP 安装；带引号扩展值暂明确拒绝，不伪造支持。解析结果不等于已安装安全关联，不修改生产 Go 行为。生产通信所有权仍 9/15（约 60%），无 DMG。

### 1.98：原生注册事务编排

- 基线：REGISTER 发送、异步 AKA 和认证头构造尚需调用者手工串接。目标：一个拥有连接的 future 完成初始请求、挑战、认证重发及注册返回，所有阶段共享截止时间和取消信号。
- `RegistrationConnection::register_with` 消费连接，调用模板与认证回调，受既有挑战次数上限约束，CSeq 检查递增；任何错误或 future 被丢弃即关闭连接，不暴露半注册流。成功返回同一连接，保留缓冲中的后续 SIP 请求。
- 安全边界：继续拒绝未安装 sec-agree 的连接，不将 ePDG ESP 误认为 IMS IPsec；普通路径不保留 CK/IK。模板回调必须非阻塞，负责新 Via branch、运营商策略及必要的多保护域认证维护。
- 测量：本地 duplex 贯通完整 REGISTER→401→模拟 SIM AKA→认证 REGISTER→200→INVITE 保留；回归 IMS/VoWiFi 和 Rust Release 构建。无新增依赖，复用已有阶段降低状态分散成本。
- 未接入真实运营商模板/安全关联与守护进程生产会话，不能认定真机注册完成；生产通信所有权仍 9/15（约 60%），不生成 DMG。

### 1.97：有序 REGISTER 请求编码

- 基线：注册连接只消费外部拼接字节，缺少可复用请求构造边界。目标：保持模板头字段顺序，集中 Content-Length/事务字段校验，认证头与模板冲突明确失败。
- `register_request` 编码完整 REGISTER，调用方仍负责运营商 Contact/PANI/Security 等策略值。要求核心字段、拒绝 CRLF 注入和重复核心字段，复用真实发送路径的事务验证，不另造解析逻辑。
- 测量：二进制正文长度、模板顺序、注入/重复拒绝；现有流式 401→200、注册后 INVITE 保留及部分写取消测试改用构造器生成请求。IMS/VoWiFi 回归与 Rust Release 构建验证。
- 取舍：一个小型编码层替代未来各调用点字符串拼接，低成本集中边界约束；尚未迁移运营商模板生成器和真实注册编排，不能据此接管生产。生产通信所有权仍 9/15（约 60%），本轮无 DMG。

### 1.96：IMS 原生读卡身份适配

- 基线：IMS 异步挑战仅有通用回调，守护进程身份绑定读卡接口返回裸 AKA 结果。目标：复用同事务 ICCID/IMSI 校验，提供直接兼容 IMS 回调的接口；身份改变时保留明确错误。
- `ReaderAkaBinding::authenticate_ims` 使用既有 PC/SC 身份验证路径，泛化内部工作线程返回类型。在工作线程内封装 Zeroizing RES/CK/IK/AUTS，使等待者取消后返回的结果仍可在丢弃时清除；不声称消除底层所有栈副本。
- 验证：异步 IMS 挑战→绑定 reader/ICCID/IMSI→模拟硬件结果→认证头/IK，及 IdentityChanged 不生成认证头；同时回归既有双 reader 独立绑定。未触碰真实 SIM，不改变生产运行时。
- 取舍：复用原有任务锁和 35 秒硬件截止时间，避免新建并行读卡通道，成本为小型类型适配。该接口待注册编排调用，不能视作真机注册完成；生产通信所有权仍 9/15（约 60%），不生成 DMG。

### 1.95：异步 SIM→IMS 认证衔接

- 基线：原生认证头只能接收调用者预先取得的 RES/AUTS，尚无可取消的读卡等待阶段。目标：挑战解码→异步 SIM 回调→认证头/密钥结果，统一绝对截止时间，取消后不开始硬件操作。
- 新增 `aka_exchange`，复用挑战选择器；SIM 回调必须由调用者绑定不可变 reader/SIM 身份。成功保留 Zeroizing CK/IK 供后续安全关联安装，同步失败产生 AUTS，拒绝和设备错误明确返回；不创建后台游离任务。
- 代价/取舍：暂以一次无秘密的丢弃式 Digest 计算预检所有输入，避免畸形输入触发真实 SIM；少量重复计算换取单一校验来源，不新增依赖。后续结合实际调用量再决定是否拆分已验证输入类型。
- 验证：模拟 SIM 检查 RAND/AUTN、成功密钥返回、等待超时和预取消；IMS/VoWiFi 回归及 Rust Release 构建。未进行真实 SIM/运营商测试，也未接管生产注册。
- 生产通信所有权仍 9/15（约 60%），下一步为守护进程身份绑定适配与注册事务编排，保留 Go 后端，本轮不生成 DMG。

### 1.94：注册认证保护域绑定

- 基线：注册跟踪器混合查找 WWW-Authenticate 与 Proxy-Authenticate；认证头构造仅接受裸挑战值，调用方可能选错保护域。
- 目标/测量：401 只接受 WWW-Authenticate 并产生 Authorization；407 只接受 Proxy-Authenticate 并产生 Proxy-Authorization。新增两种状态的正确/错误来源及多 AKA 挑战拒绝测试，运行 IMS/VoWiFi 回归与 Rust Release 构建。
- 实施：认证头结果携带固定头名称，按响应状态选择来源；多个可用 AKAv1 挑战不擅自选择 realm；跳过其他认证方案，畸形 Digest 明确失败。复用原有解析器，无新增依赖，低成本修复认证路由歧义。
- 尚未完成：真实注册事务自动生成、SIM 挑战执行、sec-agree 安装和实时 SIP 分发。生产通信所有权仍 9/15（约 60%），本轮不改变 UI、不删除 Go 后端、不生成 DMG。

### 1.93：原生 AKA Authorization 构造

- 基线：参数解析和 Digest 数学独立存在，尚无从挑战生成认证头的接口。目标：补齐该运行时必需环节，完整 auth 请求头与 Go 差分样本逐字一致，正确处理 opaque 转义及 AUTS，不允许退回普通 MD5。
- 新增 `aka_authorization`：支持无 qop、auth、auth-int；同时提供时优先 auth；透传并转义 opaque，拒绝未知算法、不支持的 qop/userhash/charset 和超长输出。调用方负责新鲜 cnonce、nonce count 和 401/407 对应的头名称。
- 测量：IMS 23 项、VoWiFi 68 项及 Go simauth 测试通过；覆盖完整头样本、AUTS、转义和拒绝路径。独立函数复用现有解析及计算，无新增依赖，收益超过小模块维护成本。
- 下一步仍需接入注册事务、真实 SIM、sec-agree 及异步来电/短信分发；本轮不改变运营商模板顺序与生产通信所有权（9/15，约 60%），不删除 Go 通信后端，不生成 DMG。

### 1.92：Digest 挑战参数边界

- 基线：AKA 挑战检测以逗号直接拆分，无法区分 quoted-string；引号内的 `algorithm=` 可能被误判为真实算法。
- 目标与测量：有效 AKAv1/v2 检测保持兼容；带逗号/转义参数正确解析，重复参数、未闭合引号、尾随垃圾及控制字符明确拒绝；通过 IMS/VoWiFi 回归和 Rust Release 构建验证。
- 实施：共用有限解析器，限制头值 8192 字节、参数 32 个；参数值使用 Zeroizing 存储，不引入依赖。收益是移除认证边界歧义，成本仅一个内部模块，达到本轮实施阈值。
- 边界：本轮仅完善挑战识别与解析，不代表 Authorization 序列化、SIM 驱动接入、IMS sec-agree 安装或运营商注册已完成。生产通信所有权仍为 9/15（约 60%）；保留 Go 通信后端，不生成 DMG。

### 1.91：IMS AKAv1-MD5 Digest 数学

- 基线：1.90 只有 nonce 解码，实际 Digest 仍由 Go simauth 生成。目标是迁移确定性的认证数学并与已运行实现对照，暂不重写运营商 Authorization 参数排列。
- 实施：compute_aka_digest 仅接受明确 AKAv1-MD5；RES 使用原始 4–16 字节（含 NUL），无 qop/auth/auth-int 分别计算，auth-int 对原始 body 哈希。同步失败使用空密码并输出 14 字节 AUTS 的 base64。qop 计数必须大于零，返回摘要/AUTS 使用 Zeroizing 且不实现 Debug；不调用 SIM，不重复 AKA。
- 成本/收益：新增固定 md-5 0.10.6，复用现有 digest 0.10 与 base64；MD5 仅用于运营商明确协商的 AKAv1-MD5，不作为一般安全算法建议。拒绝普通 MD5 和未实现 AKAv2-MD5，无降级。确定性数学迁移成本低，可直接服务后续真实认证，值得实施。
- 差分：Go ComputeDigestWithAKA 与 Rust 对同一含零字节 RES、原始 nonce、固定 cnonce/nc、含零字节 body，在三种 qop × 正常/同步失败六个组合上匹配固定摘要；两边均断言而非仅输出日志。AUTS 编码也校验。
- 规范依据：https://www.rfc-editor.org/rfc/rfc3310.html §3.3–3.4（RES 密码字节、同步失败空密码和 AUTS）。
- 回归：Go simauth、IMS 20 项和 VoWiFi 68 项测试通过；1.91 Rust Release 构建通过。
- 边界：尚无完整挑战参数解析、Authorization 模板输出、真实 SIM 绑定调用及 sec-agree 安装；尚未接生产注册，不能据此宣称 IMS 可用。整体约 60%，无 DMG。

### 1.90：AKA nonce 兼容与安全传输门禁

- 基线：Go simauth 已处理运营商多种 nonce 格式，Rust 尚未具备；原生连接会根据 Security-Server offer 返回 ipsec=true，但没有实际 IMS sec-agree 安装器，可能误报。
- 目标/取舍：先迁移可独立验证的 nonce 解码，复用已有 base64/zeroize；对尚未实现的安全传输明确报错而非默认降级。收益是避免错误 SIM 输入和虚假安全状态，不改 Go 已验证生产路径。
- AKA：支持 hex-first、标准/URL-safe base64、带/不带 padding，与 Go 保持格式优先级；提取前 16 字节 RAND、后 16 字节 AUTN，允许后续 server data；原始 nonce 仍由调用方用于 Digest，不把转换后字节当作 nonce。限制输入 4096 字节，拒绝不足 32 字节及错误编码；敏感输出不实现 Debug 并在释放时清零。
- 差分夹具：Go/Rust 使用相同 35 字节 RAND/AUTN/server-data 夹具，在 5 种编码下比对分割结果；Go simauth 测试通过。
- sec-agree：仅在原生 RegistrationConnection 中，AnswerAka.install_ipsec 或 Registered.ipsec 为 true 时返回 SecurityTransportUnavailable，由现有失败 guard 关闭连接。不把 ePDG ESP 当成 IMS 内层安全传输；不影响 Go 安装器或显式 disabled 策略。401/200 两种含 Security-Server 响应均不能移交为已注册。
- 回归：IMS 19 项、VoWiFi 68 项及 Go simauth 测试通过；1.90 Rust Release 构建通过。
- 待办：AKA digest/Authorization、真实安全传输安装与网络切换仍未完成；门禁不是功能完成。生产迁移仍约 60%，无 DMG。

### 1.89：可靠 REGISTER 发送与连接所有权

- 基线：1.88 只等待响应，生产请求模板仍由 Go 构造。目标是可靠流上的发送/等待一体化，避免部分写入后重用连接，也保留注册后的来电流。
- 实施：RegistrationConnection 拥有读写半流和 tracker；初始为 Idle，请求 Content-Length、Call-ID、CSeq 校验后 write_all/flush，完成才转入 AwaitingInitial/Authenticated。一个绝对 deadline 覆盖发送和收包；请求模板/鉴权仍由会话 owner 提供，不篡改运营商参数。
- 失败：错误、超时或调用方取消时 RAII guard 标记 Failed 并释放两半流，不允许残余 TCP 字节参与下一事务。成功可移交相同 reader/writer，保留已经读入的 INVITE/SMS 数据。
- 收益/成本：复用 reader、tracker、Tokio split，无新依赖；新增单一连接 owner 取代未来业务层重复编写发送取消处理。未动现有 Go 注册路径。
- 动态验证：本地双向流先读完整 REGISTER 再回复 401/200，状态转换正常，200 后连续 INVITE 保留；8 字节写缓冲制造部分写入阻塞，取消后对端只读到 8 字节并收到 EOF，连接不能移交为已注册。
- 回归：IMS 17 项、VoWiFi 68 项测试通过；1.89 Rust Release 构建通过。
- 边界：此处模拟 401/200，未计算实际 IMS AKA authorization，未安装 sec-agree IPsec，未进行运营商注册。需要将当前连接 owner 接到原生 TCP/ESP、运营商模板与鉴权实现后再切换生产。整体约 60%，无 DMG。

### 1.88：REGISTER 异步等待与状态失效

- 基线：1.87 完成 SIP 流读取，但注册状态机尚未与异步收包连接。目标为单一 reader 消费注册响应，保证来电/其他事务不被吞掉，取消及断链后不继续保持等待或成功状态。
- 实施：await_registration 仅把匹配 Call-ID/CSeq 的 REGISTER 响应交给既有 tracker；请求、其他事务及 CRLF 交还有界事件通道，通道关闭/满明确失败。绝对 deadline 不被 1xx 或保活延长。RAII pending guard 在 timeout、错误和调用方丢弃 future 时将本次注册标记 Failed；401/407 或成功响应正常返回时保留状态。
- 收益/成本：复用现有 reader/tracker，无新依赖和任务，不重写生产注册模板；消除异步读包与状态机之间的空档，成本仅一个小型事务适配模块。
- 动态验证：同一流混合来电 INVITE、其他 Call-ID 响应、100、401、200，确认来电/其他响应保留、401→authenticated→200 状态转换；调用方取消和 EOF 标记失败；持续 100 响应不能延长固定 deadline。
- 回归：IMS 15 项、VoWiFi 68 项测试通过，1.88 Rust Release 构建通过。
- 边界：测试由本地输入模拟响应，未执行真实 REGISTER 发送、SIM AKA、sec-agree IPsec 安装或运营商注册；事件消费者仍须接会话业务层。暂不切换生产 Go owner，整体约 60%，不输出 DMG。

### 1.87：IMS TCP 消息定界

- 基线：SipResponse::parse 只接受完整消息，而 TCP 一次读取可能只有半包或包含多条信令。目标是给 1.86 stream 提供有界、取消安全的消息读取，不把 TCP 可用等同 IMS 注册成功。
- 实施：新增 SipStreamReader，按 RFC 3261 §18.3 的 Content-Length（含紧凑名 l）定界；请求/响应均保留原始消息供业务解析；重复/缺失/非法长度、超限和半包 EOF 失败后不尝试重新同步。沿用 64 KiB 消息上限、128 个头和 8192 字节行上限；增量扫描头边界，保留连续消息余量。每个前导 CRLF 单独上报，ping/pong 关联仍由会话 owner 实现，不视为在线证据。
- 成本/收益：复用已有 Tokio/zeroize，无新增协议栈依赖；减少复制出错及拆包风险，成本为单个模块。缓冲与读取 scratch 使用 Zeroizing；等待 read 被取消时，既有半包仍保存在 reader 内，后续可续读。
- 动态验证：逐字节分段、连续消息、二进制 body 内含 CRLF、非法长度、重复头、截断 EOF、取消后续读；进一步把 framer 接到 1.86 用户态 TCP 测试，回显 5000 字节二进制 SIP MESSAGE 并比较完整原始消息。
- 验证结果：IMS 12 项、VoWiFi 68 项测试通过，1.87 Rust Release 构建通过。
- 边界：仍未驱动真实 IMS REGISTER/401 鉴权及来电事务；请求语义、保活关联、绝对事务期限由后续 IMS owner 负责。当前只完成可靠流定界，不是完整 SIP 栈。保持生产 Go owner，整体约 60%，不输出 DMG。
- 参考：https://www.rfc-editor.org/rfc/rfc3261#section-18.3

### 1.86：用户态异步 TCP stream

- 基线：1.85 可通过回调收发 socket，但 IMS 尚不能使用标准异步读写。目标是在不创建宿主 socket、不引入新运行时依赖的前提下，为现有 Rust IMS 提供 AsyncRead/AsyncWrite；复用 Tokio duplex 和 smoltcp，成本低于重新实现流协议，值得实施。
- 实施：TcpStreamDriver 安装会话内 TCP socket，上层通过 ConnectingTcpStream::connect 等待真实握手完成。驱动以 into_service 消费自身，绑定网络 future 生命周期；失败/退出释放 pending connect 与 duplex，避免上层永久等待。连接失败保留 io error kind，30 秒建连上限；发送与接收均按可用缓冲推进，Waker 通过已有 watch 唤醒网络循环。
- 缓冲：TCP RX/TX 各 16 KiB、duplex 每方向 16 KiB，无无界流缓存；未声称 Tokio/smoltcp 内部流缓冲具有秘密清零保证。driver 不持有宿主网络连接。
- 动态测量：两个用户态网络跨不同网段，应用 await connect、write_all 5000 字节、read_exact 回显并逐字节比对，跨 MTU TCP 分段通过；无监听端口返回 RST 时，网络任务报错且 pending connect 结束，不永久挂起。两种情形均有 5 秒测试期限。
- 回归：VoWiFi 68 项测试通过（0.46 秒），1.86 Rust Release 构建通过。
- 边界：正式 IMS REGISTER 状态机尚未接该 stream；TCP over ESP 联合验证、断网重传/半关闭及多会话隔离仍待完成。未切换生产 owner，不改 UI、不生成 DMG，整体仍约 60%。

### 1.85：ESP 与用户态网络栈联合生命周期

- 基线：隧道与用户态网络栈分别可运行，但尚无联合 owner，现有全链路 fixture 仅交换手工构造 IP 包。
- 目标/收益门槛：不增加线程或后台常驻任务，复用既有两端 future 与有界队列，使业务 socket 实际通过选定的加密代理通道；任意一端结束都释放另一端。收益直接对应后续 IMS 接入，成本仅一个组合入口和升级已有动态 fixture。
- 实施：NativeTunnel::run_network 从当前隧道的 CP 配置创建 TunnelNetwork，连接双向 64 包队列，持有并选择两个 future；退出时释放 socket、队列、SA 密钥与代理 transport。优先保留已认证的 DELETE/DPD 错误，避免被附带的通道关闭错误覆盖。
- 动态证据：本地 SOCKS 域名转发→SA_INIT→EAP→AUTH→ESP 后，smoltcp UDP socket 发送 REGISTER 文本；对端解密后由另一个用户态 socket 校验并接收，回复 SIP 文本，客户端实际读取并回送收讫消息，最后确认 IKE DELETE 应答。fixture 不再修改 IP 字节来伪造回复，IP/UDP 校验和由真实栈处理。
- 故障注入：应用回调主动失败，组合运行返回 Application 错误；对端观察 SOCKS 控制连接 EOF，随后 80ms 无额外 UDP。两个情形合计约 0.38 秒通过。仍是本地协议/生命周期测试，不是运营商 REGISTER 成功。
- 回归：VoWiFi 67 项测试通过（0.48 秒），1.85 Rust Release 构建通过。
- 待办：正式 IMS 异步 socket API、TCP 分段/重传及联合隧道测试、运营商兼容和真机验证。未切换 daemon 生产 owner，不删除 Go。整体生产迁移仍约 60%，不生成 DMG。

### 1.84：分配地址与用户态运行循环

- 基线：1.83 只有 IP-medium 设备，测试手工驱动，尚无 CP 地址配置和异步收发 owner。
- 目标/成本门槛：复用既有 NativeTunnel 通道类型，实现一个拥有 interface/socket set 的 future；不建立系统网卡或另一套代理。收益是形成 IMS socket 的运行承载层，成本为单个运行模块，不新增第三方依赖。
- 实施：TunnelNetwork 从 TunnelConfiguration 配置 IPv4 /32、IPv6 指定前缀及各自点到点默认路由，仅作用于用户态栈。以独立随机种子创建 interface；超出地址/路由容量明确拒绝，不静默丢配置。
- 运行：单个 future 拥有 socket 集合，限制最多 16 个；非阻塞 service 回调处理应用收发，watch 唤醒应用工作。按栈 poll_delay 驱动定时器，限制 1ms–1s；数据只通过 IP 队列出入。发送等待有 5 秒期限且可取消，通道关闭返回错误，stop 或调用方 drop 释放网络栈，不分离后台任务。
- 动态测量：不同网段地址 10.0.0.2/32 与 10.20.30.9/32 经异步队列完成 TCP 建连及 TCP/UDP 双向字节交换，随后取消两端并校验接收通道已释放；关闭输入通道返回 TunnelClosed。VoWiFi 67 项回归通过（0.30 秒），Rust Release 构建通过。
- 边界：尚未由 NativeTunnel 联合驱动，service 回调不是最终 IMS 异步 socket API；尚待联合 ESP 测试、双会话隔离、TCP 分段/重传和真机注册。现有 Go 生产运行时不变，不发布 IMS/SMS 就绪，不删除 Go。整体仍 9/15（约 60%），不生成 DMG。

### 1.83：用户态 TCP/IP 栈 IP 设备接入

- 基线：NativeTunnel 只提供 IP 队列，不能直接承载 IMS socket。收益目标是复用 TCP 重传/校验和/协议处理，避免自行编写 TCP 栈；不增加任何宿主公网出口。
- 选型：固定 smoltcp 0.14.0，仅 std、medium-ip、IPv4/IPv6、TCP/UDP，关闭默认 feature。接口依据 https://docs.rs/smoltcp/0.14.0/smoltcp/phy/trait.Device.html ，许可保存在 THIRD_PARTY_NOTICES.md。
- 实施：netstack_device.rs 的 TunnelIpDevice 为每个隧道独占，双向各最多 64 包，IP MTU 1280；发送队列满时停止授予 TX/RX token，输入满返回错误，不无界积压；close 清空并终止收发。包缓冲释放时清零，保留软件校验和检查。不包含系统 socket、DNS 或路由操作。
- 动态验证：两个用户态 interface 经 IP 队列进行 IPv4/IPv6 TCP 建连及 UDP 双向 REGISTER 文本收发；这只是字节传输，不是 SIP 注册验收。队列满及关闭用例通过。VoWiFi crate 共 65 测试通过（0.28 秒）；Rust Release 编译通过。
- 成本：该 feature 集合的普通依赖树新增 smoltcp 及 bitflags 1.x、byteorder、heapless/hash32、stable_deref_trait、managed，复用已有 cfg-if；Cargo.lock 另含未启用可选依赖，不代表全部进入运行时。正常 MTU 下设备队列有效载荷预算最多 160 KiB/会话，另加队列元数据和 socket 缓冲。尚未生产引用，当前 daemon 体积不能用于估计最终启用增量，接入运行循环后再实测。
- 结论：初步验证通过，继续候选集成；不得宣称 Rust IMS 可用。待完成 CP 地址/路由配置、异步 socket 与取消、多会话隔离、TCP 分段/断链、隧道联合测试及真机验收，之前不切换 Go。版本 1.83，无新 DMG，整体生产所有权仍 9/15（约 60%）。

### 原生运行时接入门槛（2026-09-24，未切换生产）

本轮核对 startup.rs、child_data.rs、daemon 启动路由及 IMS crate：NativeTunnel 的业务接口仍为内部 IP 包队列；IMS crate 提供 SIP 编解码和 REGISTER 状态机，但尚没有连接该队列的用户态 TCP/UDP socket 层。不得把 IKE/ESP 成功直接发布成 IMS/SMS 可用，也不得为演示启动而绕过该缺口新增公共开关。

下一实施顺序：

1. 评估成熟 Rust 用户态 TCP/IP 栈，以 IP-medium 设备对接 NativeTunnel 的收发队列。禁止绑定系统网卡、修改系统路由或让 IMS socket 走宿主公网。现有代理 transport 始终是唯一外层出口。
2. 每个会话独占网络栈、地址、端口和 socket 集合；由 CP 分配地址，P-CSCF 为远端。先贯通 UDP 和 TCP 两种 SIP 传输，不把仅 UDP 成功当作可替换 Go。
3. 接通已有 REGISTER 状态机，再接 SMS 和 PCMA/RTP；独立发布隧道、IMS、短信和通话状态。停止/DPD 失败必须关闭内层 socket、取消等待并撤销就绪状态。
4. 最后才把固定 reader/SIM 的 Rust AKA provider 接入生产会话启动入口；保持互斥接管，绝不并行启动 Go 和 Rust 两个真实 SIM 会话。

测量与收益门槛：以当前 Rust SOCKS→SA_INIT→EAP→AUTH→ESP→DELETE 本地运行用例作为隧道基线。候选网络栈必须通过本地 TCP REGISTER 往返、UDP SIP 往返、断链取消，以及两个会话同端口但不同地址的隔离用例；队列有界、关闭后没有遗留收发任务。先记录延迟、内存和依赖体积，再决定引入。若必须自写 TCP 重传/拥塞控制或更改稳定硬件驱动才能完成，则停止该候选方案，保留 Go 数据面，不以迁移比例为由接受回归。运营商兼容、真实 SMS/PCMA 仍须独立真机验收。

当前取舍：不增加未被生产调用的适配壳或仅为测试存在的启动服务，不删除 Go 业务实现。本次为接入路径核实及计划修正，版本保持 1.82，不生成 DMG。

### 1.82：EAP-AKA 卡片身份绑定

- 基线：1.81 已修复锁顺序，但 EAP-AKA 仅绑定 reader/application，物理卡身份和同步重试仍未绑定。
- 目标/测量：生产 Go 请求传递缓存 ICCID/IMSI；Rust 在同一 PC/SC 事务中先比对身份再 AUTHENTICATE；同步失败重试不可更换或删除身份绑定。以生产 manager→Unix socket 请求测试、PC/SC 脚本测试和 exchange 重试测试验证，不以模拟替代真机验收。
- 取舍：复用既有 Rust identity-bound AKA，不引入另一套硬件连接和状态机；额外读卡开销只发生在鉴权，防止串卡收益超过成本。缓存缺失时保留原兼容路径，BLE 不改变，不宣称所有认证入口已强制身份绑定。
- 实施：EAP 请求增加可选 expected_identity；同步上下文保存首次绑定（包括无绑定状态），拒绝身份变更/降级；身份冲突返回 HTTP 409；共享身份 DTO 在释放时清零。
- 验证：Go TestNativeReader 系列 race 测试、Rust reader_ 17 项、EAP identity 9 项、PC/SC 身份绑定脚本测试通过；1.82 Rust Release 构建通过。未部署、未真机鉴权；无新增 DMG。生产所有权迁移仍为 9/15，约 60%。

### 1.81：生产 EAP-AKA 锁顺序修复

- 基线：标准 AKA 已避免 authMu→mu 阻塞，但 EAP-AKA 仍阻塞获取 mu；卡信息刷新持有 mu 等待 authMu，可形成死锁。
- 目标及成本门槛：共享一次非阻塞 reader/generation 校验，消除反向等待；不改 UI、认证协议或硬件驱动。收益为解除认证/切卡永久阻塞风险，成本为一个内部函数和针对性测试，值得实施。
- 实施：两条认证入口均在持有 authMu 后调用 validateAuthenticationReader，状态锁被占用时返回 reader busy，由已有 defer 释放认证锁；继续拒绝 reader/generation 不匹配。
- 测量：go test -race ./cmd/proletarius-macos -run 'TestNativeReader' -count=1 通过；测试覆盖持锁冲突及时返回、释放后恢复、错误 reader 和 generation 拒绝。仅有既有重复链接 libusb 警告。
- 边界：未部署或进行真机认证；EAP-AKA 的 ICCID/IMSI 事务绑定尚待完成。本轮不输出 DMG。生产所有权迁移仍为 9/15，约 60%。

### 1.80 集成构建及发布验证

- macOS 27 arm64 Release 集成构建通过，应用及嵌入 Rust 后端为 arm64；版本 1.80，最低系统 27.0。
- 按用户要求已生成 Desktop/test/Proletarius-1.80-macOS27.dmg，本机签名验证及 hdiutil 镜像校验通过；未公证、未使用 UPX，未进行真机通信验收。

### 1.80：生产 Go→Rust AKA 身份绑定接入（2026-09-24）

- 基线：Go 管理器调用 Rust AKA 未发送 expected_identity，前两轮身份绑定尚未作用于正常管理器认证路径。
- 实施：管理器在连接快照中复制非空 ICCID/IMSI，标准 PC/SC AKA 请求携带 expected_identity；取得 authMu 后再次核对连接代次/读卡器。此时仅 TryLock(mu)，避免与持有 mu 等待 authMu 的身份刷新形成反向死锁，竞争时明确返回 reader_busy。
- 测量：本地 Unix socket 模拟 Rust 端，直接调用管理器认证方法，验证收到所选身份且 ISIM unavailable 原有处理仍成立；Go cmd 全包测试通过。未使用物理号卡。
- 收益阈值：把已实现保护接入现有调用链，避免继续停留在可选但无人调用状态；增加的只读身份 APDU 成本需真机量测，目前不声称延迟改善。
- 边界：缓存身份缺失时保留旧兼容调用；BLE、独立 EAP 适配端点和生产隧道所有权未迁移。未来仍需完整 generation 与断开生命周期接入，不能据此宣称多卡并发或全部认证路径已保护。未改 UI、未生成 DMG，所有权迁移约 60%。

### 1.79：固定身份的异步 SIM 认证绑定（2026-09-24）

- 基线：服务侧绑定读卡器/身份/应用并调用阻塞操作的逻辑仍嵌在 HTTP handler 中，原生隧道无法直接复用。
- 实施：ReaderAkaBinding 以不可变 Arc 固定读卡器、ICCID/IMSI、USIM/ISIM，异步认证复用 reader_task 和同事务身份核对。已有 expected_identity API 分支已接入，旧无绑定请求保持兼容；身份与挑战缓存使用 Zeroizing，不派生 Debug/Serialize。
- 测量：两个不同绑定并行模拟认证，验证 reader/身份/应用/RAND/AUTN 不串用，身份变化错误保留；17 项 reader_ 服务测试通过。
- 同步修正 ownership 能力说明，删除陈旧的“仅一轮 IKE_AUTH”描述；owner/state 仍为 Go/protocol-shadow，未虚增生产迁移进度。
- 成本收益：抽出实际被 API 使用的可复用绑定，减少下一步 daemon provider 接入重复逻辑；不是多卡待机或真机验收。现有 Go 调用仍未主动传 expected_identity，需结合连接 generation/身份缓存接入，不能声称所有生产 AKA 已获得新保护。
- 未改 UI、未生成 DMG；生产所有权迁移约 60%。

### 1.78：认证后台任务取消与所有权（2026-09-24）

- 基线：AKA/EAP-AKA 两处各自 spawn_blocking，超时显式 abort，但整个请求 future 被丢弃时不会取消仍在排队的操作。
- 实施：共享 reader_task::run；Drop guard 取消尚未执行的 blocking task，保留现有 35 秒超时与错误映射。已执行 PC/SC 操作不会被假装终止，锁及 transaction 随 worker 保持到真实返回。两个现有认证 API 已使用此实现，而非新增未调用 helper。
- 测量：单 blocking worker 排队后丢弃等待方，验证物理操作闭包未执行；运行中取消和超时均验证锁仍持有，收到释放信号后才解锁；16 项 daemon reader_ 测试通过。测试使用同步闭包模拟硬件阻塞，不访问真卡。
- 收益阈值：去除两处重复控制逻辑并修复真实取消生命周期缺口，为后续隧道 SIM provider 复用；没有增加 UI/接口表面积。
- 仍待原生隧道 provider 与 daemon 启动路由接入。保留 Go、不改 UI、不生成 DMG；生产所有权迁移约 60%。

### 1.77：PC/SC AKA 卡片身份绑定（2026-09-24）

- 基线：native AKA 串行锁仅按读卡器名称绑定，读卡器内 Profile 已变化时仍可能执行旧会话挑战。
- 实施：新增 authenticate_aka_auto_for_identity，在同一读卡器锁及 PC/SC transaction 内读取 ICCID/IMSI、核对期望身份，再选择应用执行 AKA；不一致返回无敏感数据的 reader_identity_changed。现有 AKA API 可选 expected_identity，身份变化映射 HTTP 409；未传入时保持旧接口兼容，不擅自改变生产调用行为。
- 测量：原有 APDU 脚本扩展覆盖 ICCID 变化、IMSI 变化均未发送 INS 0x88，匹配身份仅发送一次；16 项 reader 与 4 项 daemon reader_auth 相关测试通过。
- 收益阈值：复用既有设备锁及事务，增加有限只读 APDU，收益是防止卡片切换后串用身份；不扩展为全项目审计。
- 边界：保护当前 PC/SC 认证事务，不等同于持续设备 generation 监测，也未涵盖 BLE 传输；服务侧原生隧道 provider 接入仍待完成。未操作真卡，保留 Go、不改 UI、不生成 DMG，生产所有权迁移约 60%。

### 1.76：服务侧配置化启动与随机参数所有权（2026-09-24）

- 基线：上层需自行提供 SPI/Nonce 和两份独立 IKE/EAP 身份，容易固定测试值或误配身份。
- 实施：start_configured_native_tunnel 统一消费有效设置，使用 OS CSPRNG 生成每次启动的 IKE SPI、CHILD SPI、32 字节 Nonce，拒绝零 SPI；同一身份用于 IDi/EAP。原始报文启动函数缩为 crate 内部。临时认证请求帧使用 Drop 清理，包含取消路径。
- 测量：完整 SOCKS/NAT-T/EAP/AUTH/ESP/DELETE 场景改用公开配置入口，模拟 ePDG 从请求读取随机值派生实际密钥及 ESP SPI，63 项 VoWiFi 测试通过。
- 收益阈值：取消上层重复安全参数构造和身份一致性责任，不引入额外网络行为；配置字符串仍只允许原生已实现能力，不隐式覆盖运营商配置。
- 仍待实际有效设置适配、SIM provider/设备 generation 与 daemon 接入；未调用真实 SIM，不代表运营商验收。保留 Go、不改 UI、不生成 DMG，生产所有权迁移约 60%。

### 1.75：原生算法提议配置编码（2026-09-24）

- 基线：原生启动仍使用手写 SA 提议，不能直接消费 strongSwan 风格的配置字符串。
- 实施：build_gcm_proposals 编码明确支持的 AES128/256-GCM16、SHA PRF、MODP2048、ESP noESN；保留配置顺序与编号，同一列表只要有不支持项就整体报错，不筛掉条目、不混入默认提议。初始 IKE SPI 必须为空，ESP 为四字节非零。
- 测量：固定 GCM IKE 字节向量在生产 Go 编码器与 Rust 新编码器均通过；顺序、重复、缺失/不支持算法检查通过；完整 SOCKS 启动场景改用配置字符串产生 IKE/ESP 提议并通过。Rust IKE 28、VoWiFi 63 项及 Go ikev2 测试通过。
- 成本收益：替换手写字节并给后续配置适配器提供能力明确的入口，不重复实现尚无数据通道支持的 CBC/ESN 等编码；这些配置保留在 Go 生产路径，不得悄悄改算法以强行切换 Rust。
- 仍待服务配置、SIM provider 与 daemon 生命周期接入及真机验收。未改 UI、未生成 DMG；生产所有权迁移约 60%。

### 1.74：首轮 IKE_AUTH 配置构造器（2026-09-24）

- 基线：原生启动调用方仍需手写身份、CP、TS 等字节，全链路测试也使用手写首轮载荷。
- 实施：build_initial_auth_frames 从明确的身份/APN、ESP 提议、地址族构造 IDi/IDr、CFG_REQUEST、TSi/TSr、EAP_ONLY；保留提议字节和顺序，不混入默认算法。校验空/控制字符身份、APN 及单个初始 CHILD 的一致非零 SPI。
- INITIAL_CONTACT 由调用方显式指定，不默认清理同身份其他 SA；不提前宣告未实现的 MOBIKE、分片或恢复能力。
- 测量：IPv4/IPv6/双栈的 CP 地址/DNS/P-CSCF 和 TS 一致性通过；上一轮 SOCKS→NAT-T→EAP→AUTH→ESP→DELETE 完整场景已改用正式构造器并通过，共 63 项 VoWiFi 测试。
- 收益阈值：消除上层手工组包接入成本，同时利用既有动态场景验收，不增加全项目静态检查。
- 仍待：运营商有效配置适配、算法字符串编码、设备身份通知政策核验和 SIM provider/daemon 接入。当前构造器不是所有 Go/运营商选项的完整替代，不能直接切换生产。保留 Go、不改 UI、不生成 DMG，迁移所有权约 60%。

### 1.73：原生启动全链路 SOCKS 本地联合验收（2026-09-24）

- 基线：1.72 新入口只验证取消，缺少完整成功衔接证据。
- 目标与测量：单一本地 SOCKS UDP ASSOCIATE 服务逐阶段验收 ePDG 域名原样传递、500→4500 且 UDP 源地址不变、MODP-2048 双方派生密钥、EAP Identity/AKA/Success、双方最终 AUTH、CHILD/TS/CP、双向 ESP 内层 IP、IKE DELETE 空确认及通道回收。单次定向测试约 0.15 秒，整体设 5 秒失败界限。
- 结果：上述完整流程通过；全量相关测试为 IKE 27 项、VoWiFi 62 项。模拟 SIM 使用公开固定测试向量，不访问读卡器、不发运营商业务、不查询真实 ePDG DNS。
- 收益阈值：一个动态联合场景替代继续堆叠孤立方法测试，直接证明下一步有效配置/daemon 接入可使用这条链路；未扩展全项目静态审计。
- 限制：双方 IKE/ESP 编解码复用本项目库，不能当作独立实现互操作或真机结果；IMS/SMS/PCMA 尚未接入此隧道。下一步配置载荷与服务层 SIM provider/生命周期接入。保留 Go、不改 UI、不生成 DMG；生产所有权迁移仍约 60%。

### 1.72：统一 Rust 原生隧道启动入口（2026-09-24）

- 基线核查：daemon 源码尚无 SaInit/IkeAuthSession/run_tunnel 调用，不能因协议方法存在就宣称生产接管。
- 目标：统一 SA_INIT→同一传输切换 NAT-T 4500→SIM EAP 回调→最终 AUTH/CHILD/TS/配置→ESP 构造；返回私有字段 NativeTunnel，绑定认证会话、ESP 与传输。启动总截止 180 秒，支持启动前及等待中取消。
- 测量：真实本地 UDP 收到 SA_INIT 后取消，以及预先取消，均返回 Cancelled、不调用 SIM provider；61 项 VoWiFi 测试通过。新入口的完整成功握手联合测试尚未完成，各阶段此前测试不能替代此门槛。
- 成本收益：连接现有实现，无新增网络依赖、DNS 或绕过代理路径；必要的生产接管前置步骤。SIM 调用取消后设备锁仍由 provider 保持，不能假定取消 future 就取消了硬件操作。
- 下一步：有效配置→请求载荷构造与服务层 SIM provider 接入、完整启动成功及失败循环、daemon 生命周期路由。暂不切换生产、不删除 Go、不改 UI、不生成 DMG；所有权迁移约 60%。

### 1.71：Rust 对端 IKE SA 删除确认（2026-09-24）

- 基线：1.70 收到认证后的 DELETE 直接失败退出，没有协议应答。
- 目标：只接受当前 SA、正确方向/序号、单个 IKE DELETE（协议 1、SPI size 0、SPI count 0）；发出加密空 INFORMATIONAL 响应后关闭 ESP。依据 RFC 7296 §1.4.1、§3.11。
- 实施：复用对端请求窗口；新增数据通道 CloseAfterIke 命令，网络 send 完成后返回明确 PeerDeleted。控制协调器有界等待发送完成，不创建独立任务，不把“入队”误当成“发出”。
- 测量：完整本地 EAP/最终认证后注入坏 tag DELETE，再发合法 DELETE；验证同序号加密空响应、明确关闭原因、收发通道关闭及退出后静默；60 项 VoWiFi 测试通过。
- 成本收益：小范围复用现有控制及发送路径，补足正常运营商关闭行为，比直接退出有明确互操作收益。生产入口仍未切换，CHILD DELETE、rekey 和真机验证仍待完成；保留 Go，不改 UI、不生成 DMG，所有权迁移约 60%。

### 1.70：Rust 控制与 ESP 生命周期联动（2026-09-24）

- 基线：探测方法没有调度，ESP 循环与 IKE 失效未联动。
- 实施：run_tunnel 消费认证会话、ESP 与已有传输；有界通道转发内层 IP，定时派发加密探测，认证对端控制包。控制和数据 future 同作用域，无 detached task；超时/发送失败/消费端关闭时释放两套密钥和 socket。控制队列拥塞显式失败。
- 测量目标：EAP/最终 AUTH 后在真实本地 UDP 上观察探测；注入坏 tag，再正常应答，下一轮停止回应，确认原密文重传、Timeout、收发通道关闭及退出后无新发包。此为本地故障注入，不是运营商真机验证。
- 收益超过成本：连接已验证的现有方法，不另建网络旁路或 DNS；直接减少孤立 ESP 生命周期风险。
- 未完成：该协调器未成为 daemon 生产入口；DELETE/rekey/非空控制载荷目前认证后明确失败，需继续实现而非宣称完整兼容。UI 离线/自动重连和 IMS 接入仍待完成。Go 不删除、UI 不变、不生成 DMG；所有权迁移约 60%。

### 1.69：Rust 主动存活检测状态逻辑（2026-09-24）

- 基线：1.68 只有被动探测响应，没有主动请求的待确认窗口、重试截止时间或失败关闭。
- 目标与测量：现有最终 AUTH 回环测试后发起空 INFORMATIONAL，坏 tag 不清除窗口、禁止并发探测、截止前不重传、截止后复用原密文、正确响应清除窗口、重复响应拒绝、耗尽重试关闭且无法重启同一 SA。59 项 VoWiFi 测试通过。
- 最终 AUTH/CHILD/TS/配置验证成功后，IKE tracker 进入 Established；仅表示 IKE 认证完成，不发布 ESP/IMS/SMS 就绪。
- 成本收益：复用现有序号窗口和密钥，增加一个有界待确认对象；直接为断链检测提供状态基础，避免新增 socket 与重复定时器框架。依据 RFC 7296 §2.1–2.4。
- 待接入：生产定时调度、命令发送失败处理、ESP 停止和上层离线/重连联动。当前单元测试使用受控截止时间，不是公网或真机长连接验收。Go 生产后端保留，UI 不变，未生成 DMG；所有权迁移约 60%。

### 1.68：Rust 对端存活探测响应入口（2026-09-24）

- 基线：数据通道可转交未认证 IKE，但没有可复用的对端空 INFORMATIONAL 响应方法。
- 目标：最终 AUTH/CHILD/TS/配置验证后才接受探测；解密认证、SPI、方向、交换类型、空载荷与独立对端序号全部匹配后响应；重复请求返回缓存密文，坏包与跳号不推进序号。
- 测量：在现有 EAP→最终 AUTH 回环测试中，验证坏 tag、正常探测、重复、跳号、下一序号及过期重放；VoWiFi 测试通过。依据 RFC 7296 §2.2、§2.4。
- 成本收益：复用现有 SK 与会话密钥，不增加 socket 或依赖，是长连接控制链路的必要小步；不增加静态审计范围。
- 边界：仅实现被动空探测方法，尚未接入生产控制调度，不表示主动 DPD、DELETE/rekey、自动重连或真机长连接已完成；其他控制载荷明确拒绝而非当作心跳确认。保留 Go，不改 UI、不生成 DMG；迁移所有权约 60%。

### 1.67：生产 IKE 分片载荷类型保留（2026-09-24）

- 基线：重组完成后读取最后到达分片的 NextPayload；正常顺序下后续分片为零，完整 EAP 响应会被解析为空。
- 目标与测量：CBC/GCM 下顺序、倒序、重复首片三个场景均只在收齐时交付一个 EAP 载荷；使用实际加密/解密及解析路径，Go race 测试通过。
- 修复：认证后缓存第 1 片的类型，与重组字节一起返回；分片集合扩容重置时同步清理类型；拒绝非法编号及后续片非零 NextPayload，按总长度预分配重组缓冲。
- 收益阈值：直接修复当前生产隧道认证丢载荷，变更仅限分片缓存与调用方，收益高于兼容维护成本。依据 RFC 7383 §2.5；不扩展为全项目静态审计。
- 未进行运营商真机验证，不代表 Rust 已接管生产 IKE；不删除 Go 运行时、不改 UI、不生成 DMG。迁移所有权仍为 9/15，约 60%。

### 1.66：已建 IKE 会话边界与发布（2026-09-24）

- 加密报文必须匹配已协商 SPI、长度与 IKEv2 主版本，拒绝明文载荷；移除认证前覆盖对端 SPI。
- CBC/GCM 篡改头、同密钥其他 SA、明文及异常后正常包回归通过；86 项 Rust IKE/VoWiFi 测试通过。macOS27 Release 及 DMG 校验通过，产物已保存桌面 test；未重启或真机验证。

### 1.65：Go SK/SKF GCM 规范修复（2026-09-24）

- 基线：1.64 已确认 Go 普通包及分片 GCM envelope 缺 Pad Length，AAD 未覆盖 SK/SKF 头；不应让此错误继续影响后续互操作验证。
- 目标与测量：普通包 AAD 覆盖 IKE+SK 头，分片覆盖 IKE+SKF generic/fragment 头；GCM 写入并移除 Pad Length，CBC 算法和填充保留。使用独立 Go 标准库 AES-GCM 解密生产封装输出、双向回环、SK/分片头篡改与截断测试，swu/crypto/ipsec race 测试通过。
- 实施：共享填充剥离函数，修正分片 MTU 的额外一字节预算，补 SK/SKF 长度边界。未增加非标准格式 fallback，未修改代理/DNS/UI，也未重启正在运行的应用。
- 取舍：标准明确且独立动态验证通过，收益超过保留已知错误的兼容成本；实网运营商 GCM/CBC、分片重组仍需真机验收，不能从单包测试推断全链路稳定。
- 依据 RFC 5282 与 RFC 7383；本轮不生成 DMG。1.64 中记录的 Go envelope 风险已在源码修复，尚未部署到运行中实例。

### 1.64：SOCKS 混合数据面验证与 IKE GCM 格式修正（2026-09-24）

- 基线：收发循环仅经过直连 IKE 回环，未与 SOCKS/ESP 联合验证。新增本地 UDP ASSOCIATE 模拟器，验证 ePDG 域名原样传递、ESP 双向密钥、坏包/重放丢弃、保活不产生业务事件、IKE 独立传递、取消及 TCP 控制连接断开。
- 测试暴露 IKE GCM 空载荷长度问题；核对 RFC 5282 §3/5.1 发现 Rust 继承 Go envelope 的两处不符合规范：缺少 Pad Length、AAD 未覆盖 SK generic header。Rust 已修正，接收支持任意 0–255 字节 padding，篡改 SK NextPayload 必须认证失败。独立 Go crypto 向量同步为规范格式；这不代表 Go swu 生产包装已修正。
- 风险记录：Go `msg_handler.go`、`fragment.go` 仍沿用旧 AEAD envelope，需要结合实际协商算法与真机报文再修正，不能拿旧差分一致当作规范正确。当前生产路径不做无证据的切换。
- 另增加发送控制命令 SK 结构门禁，避免误将明文 IKE 投入已建隧道。依据：https://datatracker.ietf.org/doc/html/rfc5282#section-5.1 。
- 取舍：动态代理联测直接发现互操作问题，收益超过测试与小范围修复成本。不改 UI、不生成 DMG；仍不等于运营商真机验收或生产 Rust 接管。

### 1.63：统一隧道收发循环与取消（2026-09-24）

- 基线：已有 ESP 对象和单包发送，但缺少统一接收所有者与上层事件出口。
- 目标与测量：同一 ePDG transport 收发 IP/IKE，保持 IKE/ESP/Keepalive 区分；畸形包可丢弃，传输错误向上传递，取消及命令队列关闭回收资源，事件队列堵塞显式失败。回环验证控制包原样传递、坏包后继续收包、取消和消费者背压。
- 实施：新增有界 mpsc/watch 驱动循环和计数，20 秒 NAT 保活、发送 5 秒上限。IKE 入站事件命名为 UnauthenticatedIke，必须由上层 IKE owner 验证，不将保活或收包当作 IMS 健康证明。
- 取舍：复用既有 transport 与 ESP，单点分流避免双读竞争；不另开代理绕行 socket。该循环尚未接入生产 owner，仍缺 DPD/换钥控制处理、真实代理混合业务验证和网络栈上层消费。
- VoWiFi 58 项测试通过；本轮不生成 DMG。

### 1.62：CHILD SA 绑定双向 ESP 与内层流量策略（2026-09-24）

- 基线：ESP 对象独立存在，认证结果未消费它，TS 仅在协商时校验。
- 目标与测量：消费 PendingChildSa 建立方向正确的 sender/receiver；出站使用远端 SPI/initiator key，入站使用本端 SPI/responder key；地址必须为已分配地址且双向包符合 TSi/TSr、协议和端口范围。加密 IKE 流程测试延伸到双向 ESP、错地址拒绝，并补协议/端口/地址族策略测试。
- 实施：新增 ChildDataPlane，密钥通过 move/take 交付避免同一待安装结果重复安装；send_ip 复用 EpdgDatagramTransport.send_esp，不开新 socket、不解析 DNS、不改变前置代理。此发送接口尚未做真实代理端到端验证。
- 取舍：接通已有三项能力而非新增平行栈，收益明确。非 GCM、无地址配置明确拒绝；IPv4 分片与 IPv6 扩展头暂拒绝，后续需要有界解析/重组策略，不能宣称所有 IP 流量可用。
- 尚无生产收包事件循环、换钥和完整服务端身份策略，不发布在线状态、不删除 Go owner，本轮不生成 DMG。

### 1.61：AES-GCM ESP 数据包与重放窗口（2026-09-24）

- 基线：Rust 已验证 IKE/CHILD 协商，但无 ESP 封解包，不能承载内层 IMS。
- 目标与测量：AES-128/256-GCM-16 封解包、IPv4/IPv6、64 包重放窗口；损坏包不推进窗口，窗口内乱序可接收，重复/过旧包拒绝，u32 序号耗尽拒绝继续发送。Go 生产加密/解包路径与 Rust 固定报文字节一致。
- 实施：复用 IKE 已有 AES-GCM 原语，密钥零化，发送/接收对象不支持 Clone，计数 IV 不重复；仅完整认证和格式校验后提交接收窗口。新实例必须使用新 IKE 派生密钥，禁止用旧密钥重置计数。依据 RFC 4106：https://datatracker.ietf.org/doc/html/rfc4106 。
- 取舍：实现初始明确支持的 GCM 数据面，无新加密依赖；CBC、ESN、SA 重建/换钥、TS 实际流量过滤和代理收发接线仍待完成。当前不能因 GCM 单测通过切换真机 owner。
- 本轮不生成 DMG，不改 UI。

### 1.60：隧道地址配置接入最终认证（2026-09-24）

- 基线：Rust 最终认证缺少 CFG_REPLY，IMS 后续无法获得内部地址、DNS/P-CSCF。
- 目标与测量：请求 CP 后必须得到唯一 CFG_REPLY；IPv4/IPv6 地址及前缀、DNS/P-CSCF 长度必须有效，至少分配一个地址且处于已协商 TSi。覆盖加密流程缺失 CP、IPv4/IPv6、前缀错误、重复/截断与 TSi 不匹配。
- 实施：配置仅存于 PendingChildSa，不修改宿主 DNS/路由；未知配置属性跳过，保留既有 Go 使用的 16390 IPv6 P-CSCF 属性。未请求 CP 的会话允许无 CP，以支持后续显式静态配置策略，但不能据此直接上线。
- 取舍：接入 IMS 必需配置、复用现有 TS 结果，收益超过独立小解析器成本。IPv6 地址当前要求标准 17 字节（含前缀），不静默仿照 Go 截断异常长度；特殊运营商响应须经证据确认再适配。
- 尚未完成 ESP 安装、服务端身份策略及真机接管；生产 Go owner 不变，本轮不生成 DMG。

### 1.59：流量选择器校验接入最终认证（2026-09-24）

- 基线：最终认证验证 AUTH 与 ESP，但未校验 TSi/TSr，不能安全授权后续内层流量。
- 目标与测量：保存首次请求 TSi/TSr，分别验证响应唯一且非空，IPv4/IPv6、协议、端口与地址区间不得超出对应报价。覆盖收窄、扩张、跨地址族、截断、重复、缺失及加密最终响应缺失 TS 的拒绝路径。
- 实施：新增有界 TS 解析，接入 PendingChildSa，不新增在线状态。依据 RFC 7296 §2.9/3.13：https://datatracker.ietf.org/doc/html/rfc7296#section-3.13 。
- 取舍：该门禁是 ESP 安装前的必要条件；当前逐条包含校验，不支持跨多个报价区间合并返回。常规 IPv4/IPv6 全范围报价可收窄，特殊合并响应需实测后扩展，不能将此限制误记为规范禁止。
- 配置地址、数据面流量过滤、ESP 和真机接管仍待完成；不切换生产 owner，不生成 DMG。

### 1.58：认证阶段保留运营商 Notify 错误（2026-09-24）

- 基线：Rust IKE_AUTH 解密后未处理错误 Notify，可能将真实拒绝误报为缺少 AUTH/SA，或继续处理同包其他载荷。
- 目标与测量：仅在 SK 认证通过后解析 Notify；错误通知保留原始类型码并关闭认证，声明 SPI 长度超过载荷必须拒绝；状态通知保留给后续处理，不误当拒绝。
- 实施：在统一 IKE_AUTH 收包路径校验，覆盖 EAP 中间轮与最终交换，不复制 SA_INIT 的握手专用逻辑。通过加密 UDP 注入 AUTHENTICATION_FAILED、正常状态通知和截断 SPI 验证，VoWiFi 51 项测试通过。
- 取舍：单点补齐错误传播，低成本避免丢失运营商失败原因；不新增 UI 或日志敏感数据，不切换生产 Go owner。TS/配置/ESP 仍待完成，本轮不生成 DMG。

### 1.57：最终 AUTH 接入 CHILD SA 选择与密钥派生（2026-09-24）

- 基线：1.56 的 ESP 提案校验和 1.55 密钥派生为独立接口，最终 AUTH 只返回载荷。
- 目标与测量：冻结首次 IKE_AUTH 中实际发送的 SA 报价；最终 AUTH 验证成功后，必须匹配唯一的响应 SA，随后使用本会话 SK_d/Ni/Nr 派生双向密钥。加密 UDP 回环覆盖成功、缺失、重复与未报价选择。
- 实施：新增 PendingChildSa 作为明确未安装结果，携带已验证选择、零化密钥及后续 TS/配置所需响应；会话继续保持 Authenticating。取消/失败消耗会话，不能重复使用半建状态。
- 取舍：直接复用既有两项实现，消除“独立接口完成但运行流程不消费”的缺口；不新增 UI、不重复协议解析，不切换生产 owner。50 项 VoWiFi 测试通过。
- 后续仍需 TS/配置、通知处理、ESP 安装与真实运营商验证。本轮不生成 DMG。

### 1.56：初始 ESP 提案选择校验（2026-09-24）

- 基线：Rust 已具备通用 SA 解析及 CHILD 密钥派生，但未校验运营商选择是否属于初始 ESP 报价。
- 目标与测量：解析原始 SA 载荷，验证单一选择、报价编号、ESP 协议、双方四字节非零 SPI、无重复 Transform、完整属性匹配；合法 GCM/CBC 返回明确密钥长度，未报价算法、错误 SPI、截断、错误组合必须拒绝。
- 实施：复用已有 SA 解析器，输出方向明确的 SPI 和密钥长度。GCM-16 包含四字节盐，CBC 必须协商完整性；初始 SA 不接受独立 PRF/DH，ESN 尚未实现则显式拒绝，不能忽略 ESN-only 报价。
- 取舍：不重复解析器或实现新 cipher，仅增加后续 ESP 安装必需的协商门禁。VoWiFi 49 项测试通过，测试覆盖合法原始载荷与变异拒绝路径。
- 此接口尚未接入最终 AUTH 响应，TS/配置与 ESP 数据面仍未完成；不宣称隧道可用或删除 Go owner。本轮不生成 DMG。

### 1.55：首个 CHILD SA 双向密钥派生（2026-09-24）

- 基线：Rust 只有 IKE SA 密钥派生，缺少 ESP 所需的首个 CHILD SA KEYMAT。
- 目标与测量：复用 PRF+，以 SK_d、Ni、Nr 生成 initiator encryption/integrity、responder encryption/integrity；与 Go 40 字节 AEAD 和 72 字节非 AEAD 固定向量一致，错误 nonce/长度/PRF 显式拒绝。
- 实施：独立零化密钥结构、Debug 脱敏、派生输入长度上限；AEAD encryption 长度包含 salt、integrity 长度为零。仅支持初始 CHILD SA，不将其复用于 CREATE_CHILD_SA/PFS。
- 取舍：复用已有 PRF+，实现成本低于维持缺失密钥接口带来的接管障碍；不增加并行协议栈或 UI。Rust 24 项 IKE 测试及 Go 差分向量通过。
- 尚未完成 CHILD SA 提案/TS/地址配置校验及 ESP 安装，因此不能据此宣布 Rust 隧道可用。本轮不生成 DMG。

### 1.54：最终 EAP AUTH 双向交换（2026-09-24）

- 基线：Rust 能生成发起方 AUTH，但未发送最终交换、未验证响应方 AUTH。
- 目标：通过既有加密传输发送 AUTH，使用原始 SA_INIT 响应、Ni、SK_pr 和 IDr 验证响应方 MSK AUTH。缺失、重复或错误 AUTH 必须失败；成功仍保持 Authenticating，不能跳过 CHILD SA 校验报在线。
- 实施：保存原始响应与 Ni、锁定会话内 IDr，常量时间比较最终 AUTH；失败或取消时消费并丢弃半建会话。删除未被调用、可直接绕过校验进入 Established 的高层接口，等待 CHILD SA 安装门禁替代。
- 测量：加密 UDP 回环验证双向 AUTH、错误 AUTH、缺失/重复 AUTH 和认证阶段保持；VoWiFi 47 项测试通过。计算公式依据 RFC 7296 §2.15–2.16：https://datatracker.ietf.org/doc/html/rfc7296#section-2.16 。
- 取舍：复用加密传输与 PRF，仅增加会话必要输入与最终交换；收益超过维护成本。此处不等于完整服务端证书/身份策略校验，也未完成 CHILD SA、ESP 和真机接管；生产 Go owner 保持不变。

### 1.53：IKE_AUTH 完整性校验先于状态提交（2026-09-24）

- 基线：未认证响应头会先消费 pending message ID，随后校验失败导致认证终止。
- 目标与测量：损坏 GCM tag、篡改 SPI 后紧随合法报文时仍成功接受原 message ID；只有损坏报文时按原重试期限关闭，不能误报认证完成。采用本地加密 UDP 故障注入，不触碰真机业务。
- 实施：先验证并解密 SK，再提交 tracker；未通过完整性/外层格式校验的报文仅丢弃，不刷新超时。已认证但内容无效的报文仍报错关闭。
- 取舍：仅调整一处收包顺序并复用既有故障夹具，避免无效报文直接打断认证的收益高于维护成本；不扩展 UI、不切换生产 owner、不删除仍必需的 Go 代码。
- 验证：VoWiFi 46 项测试通过；完整 Rust 真机链路仍未接管，本轮不生成 DMG。

### 1.52：保存认证原始输入并生成最终 AUTH（2026-09-24）

- Rust 保存实际成功的 SA_INIT 请求（包括 COOKIE 重试）、响应 nonce 与首次 IDi，供最终 EAP AUTH 计算使用，避免重建身份造成不一致。
- 加密回环测试覆盖 EAP 成功后的 AUTH 生成及错误 MSK 长度；IKE/VoWiFi 共 68 项测试通过。
- 本轮按用户要求构建 macOS 27 DMG。不切换生产隧道所有者；最终 AUTH 发送、响应认证及 CHILD SA 接管仍待完成，不能视为真机验证。

- 已由 Rust 原生负责：网关监督、PC/SC 枚举、读卡器身份与 USIM/ISIM AKA、短信/通话/传真本地归档、协议编解码、代理能力探测，以及统一迁移清单。
- 仍由 Go 硬件桥负责：DJI USB/QMI/AT、读卡器 eSIM 写卡会话，以及读卡器 VoWiFi 的实际 IKE/ESP/IMS/SMS/通话传输。
- 当前清单为 9 个 Rust 原生组件、5 个仍阻塞 Go 移除的组件，另有 1 个由 Rust 聚合但暂依赖 Go 的影子组件（约 60% Rust 归属）；该比例只表示代码归属，不代表实时业务可用率。
- `proletarius-call` 已接管 PCMA、RTP/DTMF 状态算法、确定性的 IMS 音频 SDP 协商，以及 O2/giffgaff 被叫所需的 RFC 4867 AMR-NB mode 7 payload framing。SDP 层固定为 PCMA 优先，并兼容运营商只提供 bandwidth-efficient AMR mode 7 的实际来电，同时保留动态 telephone-event 负载号。迁移门禁会逐场景比较 Go/Rust SDP 结果，并用 32 组 mode-7 payload 对 AMR packing/unpacking 做逐字节差分。AMR 编码器与实时 SIP/RTP owner 仍暂留 Go；只有完成三轮真机双向语音后才删除重复实现。
- VoWiFi 注册租约与断链重连退避已由 Rust 会话策略 endpoint 独占决策。2026-09-21 CTExcel/Surge 连续三轮真机故障注入均由 OPTIONS 超时识别失效、由 Rust 返回有界退避，并恢复完整 IKE/AKA/ESP/IMS/SMS/Call；第三轮从失效确认到 IMS/SMS 恢复约 9 秒。网络授予的 2400 秒 Contact 租约由 Rust 安排在 35 分钟刷新。三轮门禁通过后，同值 Go 计算与静默回退已删除；Rust 策略不可用会显式失败，避免双 owner 漂移。
- 1.32 为 Rust IKE crate 加入 SA key derivation：覆盖当前 Go 使用的 HMAC-SHA1/256/384/512 PRF、RFC 7296 PRF+、IKE SA keymat 顺序、AEAD 零完整性键长度，并在 Rust 密钥对象释放时清零。基线是 Rust SA_INIT 已校验提案和返回 Nonce/KE，但无可消费的 SA key material；Go `GenerateIKESAKeys` 已有对应实现。测量方法是固定 Ni/Nr/DH/SPI 与 AES-CBC/SHA256 长度，在 Go/Rust 两端比较完整 keymat 字节，并检查无效 nonce/SPI 和未支持 PRF fail-closed。Go 与 Rust 黄金向量及 Rust 单测通过，新增 4 个常用 Rust crate、锁文件 13 个传递依赖；收益超过依赖维护成本，因为这是 IKE_AUTH/ESP 实时接管的必需前置。当前派生函数仍未接入 Rust SA_INIT 运行器，DH 共享密钥计算、EAP-AKA、IKE_AUTH/Child SA 与 ESP 仍由 Go owner 负责，未宣称完成实时接管。
- 1.33 将 Go 当前唯一启用的 IKE DH 组 14（MODP-2048）迁入 Rust IKE crate。使用 `crypto-bigint` 固定宽度 Montgomery 运算对秘密指数做恒定时间模幂，以 macOS 系统熵生成指数；拒绝错误长度、1 以下/ p−1 以上和非素数阶子群中的对端公钥；私有指数及共享密钥采用清零容器。基线是 DH 完全由 Go `math/big` 实现，Rust 无 DH 能力。以固定指数派生的两个公钥与共享密钥 SHA-256 摘要作为 Go/Rust 黄金向量，并验证随机双端一致、固定 256 字节编码和异常公钥拒绝；目标均通过。新增 `crypto-bigint` 与 `getrandom`，锁文件新增 12 个依赖包；恒定时间运算、系统熵和对端子群检查是迁移生产 IKE 所需安全条件，收益高于依赖成本。该模块目前独立于 SA_INIT runner，尚未迁移会话状态或切换实时业务 owner。
- 1.34 把组 14 DH、现有 SA_INIT 提案校验及 1.32 key derivation 串为 Rust 单次握手 API。API 自动插入 KE，验证选择的 DH/加密/PRF/完整性变换，按 AES-CBC 或 AES-GCM（包括 GCM salt）取密钥长度，完成共享密钥和 `SK_d/SK_ai/SK_ar/SK_ei/SK_er/SK_pi/SK_pr` 派生。基线为各原语彼此独立。loopback ePDG stub 验证 IKE 请求载荷顺序、KE 长度、COOKIE 挑战后的相同 KE/Nonce 重用和选中 GCM key schedule；另有 AES-CBC integrity/key-length 单测。相关 Rust 测试通过，增加代码而未增加依赖；收益高于维护成本，因为它首次证明 SA_INIT→密钥派生在 Rust 单次 API 中贯通。当前仍未接入生产 VoWiFi owner，也未做真实 ePDG 测试；因此不切换默认 owner，不删除 Go IKE 业务。
- 下一切入点是把 IKE_AUTH 解密出的 EAP payload 接入现有 EAP-AKA 核心及窄 SIM AKA 适配接口，建立可限次、可取消的多轮 EAP 会话；之后补齐 IKE_AUTH 完成/Child SA 与失败清理测试。当前只有单轮加密 UDP 往返，不代表 IMS 可注册。
- 1.35 校准 Rust ownership manifest：明确 Rust 影子路径已覆盖组 14 SA_INIT→DH→key derivation，但生产 VoWiFi session、ESP/NAT-T、EAP-AKA 与 IMS runtime 仍由 Go 负责。基线是 manifest 将部分已迁移影子能力笼统写成 Go-owned crypto/transport，容易让 UI readiness 对迁移边界失真。验证以 `proletariusd` ownership 契约测试校验组件数、Go 阻塞项和比例不变（9/15，60%），改动不改变运行路由。
- 1.36 将 EAP-AKA Challenge 响应核心迁入 Rust IKE crate：接收 Go/PCSC 硬件桥读出的 RES/CK/IK 与完整 NAI，验证 Request 的 RAND/AUTN/AT_MAC，兼容现有 Go 的 IK||CK / CK||IK MK 顺序试探，生成带 AT_RES、可选 AT_RESULT_IND 与 AT_MAC 的响应，并以 zeroizing 容器处理派生密钥；FIPS 186-2 SHA-1 PRF 使用 RustCrypto 对外公开的压缩函数，不自写 SHA-1 轮函数。Go/Rust PRF 输出 SHA-256 固定向量相同；Rust 故障测试覆盖 MAC 篡改、结果长度与完整响应 MAC。此模块尚未连到 IKE_AUTH message loop，也未跨接真实 SIM 返回值/运营商，不切换 owner、不删 Go AUTH。
- 1.37 按 Go carrier 配置补齐 EAP-AKA AT_CHECKCODE 策略：`minimal/omit` 不生成，`checkcode` 仅在网络请求时基于完整 EAP-Identity transcript 生成并校验，`recompute` 强制生成；错误 transcript 与缺失 transcript fail-closed。基线是 1.36 Rust primitive 固定采用默认 minimal，无法承接常见 profile 的 checkcode/recompute 选项。收益是让迁移后的认证核保留现有 carrier override 语义；成本为一个枚举、哈希验证和窄故障样本。SIM 集成及 IKE_AUTH runtime 仍由 Go owner 负责。
- 1.38 新增 IKE SK AES-GCM-16 seal/open：按 RFC 4106 拼接 4-byte salt 与随机 8-byte IV，使用完整 IKE header 作 AAD，对通用载荷链加解密，先验 MAC 再解析；当前只接受 AES-128/256-GCM-16，其他 tag/密钥组合 fail-closed。基线为 Rust 已能生成 EAP-AKA 响应，但没有 IKE_AUTH 的 AEAD 封装/解封装；定向测量包括 Go `crypto/aes` 与 RustCrypto 完整包摘要一致、回环 round-trip、密文篡改/错误密钥/不支持算法拒绝。收益是首次能把 EAP frame 安全封装成真实 IKE SK 数据报，为 shadow IKE_AUTH 往返提供必要密码边界；代价为引入 `aes-gcm`（禁用其随机源 feature，IV 仍用既有系统熵接口）及 AES/GHASH 依赖，且暂不覆盖 AES-GCM-192、GCM-8/12 和 AES-CBC。此 codec 未连接 UDP 会话/消息 ID 或真实 ePDG，Go production owner 不变；保留取舍，后续根据现行运营商提案选择是否扩算法。
- 1.39 将 SA_INIT→IKE_AUTH shadow session 串接：由验证过的 SA_INIT 响应创建会话并从 message ID 1 开始；IKE_AUTH 请求用协商的 SK_ei 与 GCM transform 封装，响应以 SK_er 解封；超时重试复用完整相同请求字节（包括 IV），重复/过期响应不推进会话，错误报文、解密失败及耗尽重试都会关闭会话。loopback UDP ePDG stub 检查 message ID、EAP frame 往返和重试密文字节恒等。验证边界仅是本机协议影子测试：没有接入真实 SIM AKA 值、完整 EAP 多轮/Child SA，也没有真实 ePDG 或设备验证；Go 仍是生产 owner，Rust 归属比例保持 60%。
- 1.40 加入 IKE EAP payload 边界：严格提取唯一 EAP payload，限制单包 4 KiB、NAI 2 KiB，校验 EAP Request 长度/类型；Rust 生成同 ID 的 EAP Identity Response 并保留精确的 Identity 请求/响应 transcript，后续 AKA Challenge 由现有 Rust AKA 核心验证 AT_MAC/AT_CHECKCODE 并使用调用方从目标 SIM 取得的 RES/CK/IK 生成响应。NAI 与 transcript 使用清零容器；不实现/猜测 SIM 结果，也不接受 AKA'、重认证或未知 subtype。定向测试覆盖长度/重复 payload、Identity 编码、challenge 前置状态及 response frame 校验。该适配尚未连接 Rust IKE_AUTH UDP runner、PC/SC SIM service 或最终 AUTH/Child SA，生产 owner 仍是 Go，迁移比例不变。
- 1.41 扩展 AKA Challenge 到 PC/SC 的窄适配：新增 challenge 解码，只在请求同时包含 RAND、AUTN、AT_MAC 且格式有效时暴露固定 16 字节输入，供调用方交给选定 USIM 执行一次 AKA；SIM 返回 sequence synchronization failure 时按现有 Go wire 形状生成同 EAP Identifier 的 AKA-Sync-Failure/AT_AUTS，而不是把它误报为普通认证失败。测试使用含 reserved/padding 字节的 EAP Attribute 真实长度，检查 RAND/AUTN 不被尾部对齐填充污染、缺失 AT_MAC 在触碰 SIM 前拒绝、AUTS 长度错误拒绝以及响应头/ID 与 Go 结构一致。无新增依赖；尚未把解析结果接到正在运行的 PC/SC reader session，也未执行真卡挑战，生产 VoWiFi owner 不变，比例不变。
- 1.42 把 EAP-AKA 到 SIM AKA 的桥收敛为 VoWiFi 服务层可复用接口：先校验 challenge，再向注入的 reader/SIM provider 发起且仅发起一次 AKA；成功结果构造标准 AKA Challenge response，序列同步失败构造同 Identifier 的 AUTS，SIM 拒绝与读卡服务错误分开返回。AKA 结果在所有返回路径上清零，Debug 不显示 RES/CK/IK/AUTS。测试验证坏包不会触发 SIM、成功调用输入取自 RAND/AUTN、同步失败保留 ID、认证拒绝和 reader error 不混淆。该接口仍未接入 IKE_AUTH 生产循环，也不证明真实 ePDG/IMS 可用；不改变 Go owner、发布路由或迁移比例。
- 1.43 建立 EAP-AKA 的 Rust→Go 可选纵向接缝：Rust 本地路由校验 EAP Identity 请求/响应 transcript 的身份与 Identifier 绑定，解析 AKA Challenge 后对指定 PC/SC reader 执行一次 AKA，并返回 Response、MSK/K_aut 或 AUTS；Go IKE_AUTH 仅在 `PROLETARIUS_RUST_EAP_AKA=1` 时使用此响应，检查包长度、类型、Identifier 与密钥长度，默认仍走稳定的 Go 路径，未配置本地 Rust 服务时可按原路径继续。下一 re-auth identity 场景保留既有 Go 分支。Rust daemon 与 vowifi 定向测试、Go macOS 桥及 vendored `swu` EAP 测试通过，并覆盖默认关闭、malformed challenge、错配响应、同步失败和 fast-reauth 保留。此接缝不接管 IKE 多轮状态、最终 AUTH、Child SA、ESP/NAT-T 或 IMS；没有硬件真测，本组件仍 Go-owned，Rust 归属仍为 9/15（60%），不允许删除 Go EAP 业务。
- 1.44 把 EAP Identity response builder 也接入同一默认关闭的 Rust 路径。Go 仍负责选择普通 NAI/快速重认证假名，Rust 按同一 EAP Identifier 生成 Identity response；Go 校验 response Code/Type/Identifier/身份完全一致后才写入 Identity transcript，后续 AKA 可使用原字节进行 AT_CHECKCODE。Fast Re-auth Identity 继续留在 Go，保持原路径。Rust 与 Go `swu` 使用相同 IMSI→NAI 黄金向量验证 response 形状，另测坏包、响应身份/Identifier 错配、默认关闭与本地路由输入。Rust Identity 端点/Go 适配测试通过；本轮尚未启动应用，也未进行设备或运营商验证；IKE_AUTH 多轮循环、最终 AUTH、Child SA、ESP/NAT-T 与 IMS 仍属 Go，reader VoWiFi 组件 owner 不变、总体归属仍为 9/15（60%）。没有达到删除重复 Go Identity builder 的门禁。

### 1.45：EAP exchange 生命周期迁移（2026-09-24）

- 基线：1.44 的 Identity response 可由 Rust 生成，但 Go 保存完整身份请求/响应 transcript，再在 AKA 请求中传回 Rust；失败和取消没有可释放的 Rust exchange。
- 目标与测量：AKA IPC 请求不再包含 `identity_transcript`；Rust 持有至多 64 个 exchange，每项 transcript 上限 8192 字节（原始字节总量上限 512 KiB，不含容器开销）；有效期 120 秒，访问时立即拒绝过期项，额外每 30 秒回收空闲期过期数据。8 个并发消费者只允许 1 个取得同一 exchange。Go 原始 transcript 字段删除，只保留 20 字节 RFC 4187 摘要供兼容分支使用。
- 实现：Identity 返回随机 256 位 exchange ID 和校验摘要；AKA 原子取走 exchange，再调用读卡器。失败、握手提前结束、下一轮 Identity、快速重认证转回 Go 时释放剩余 exchange；错误 Identity 响应也主动释放。存储 transcript 在销毁时清零。新增依赖仅为 daemon 引用 workspace 已有 getrandom/sha1，没有新增第三方包。
- 证据：Rust workspace 327 项测试通过，release daemon 构建通过；Go macOS 桥测试及 vendored swu race 测试通过。定向测试覆盖容量满、幂等释放、过期拒绝、并发一次性消费、错误响应释放、鉴权失败不重复触发 SIM。测试和代码结果只能证明本地协议边界，不能证明运营商注册成功。
- 取舍：增加有界存储与释放接口的成本，换取删除 Go transcript 状态和重复 IPC 传输，为 Rust IKE_AUTH 状态接管提供必要生命周期。本轮不调整 UI。默认仍由 Go 驱动实时 IKE/IMS；Rust EAP 仍需显式启用，归属清单保持 9/15（60%）。
- 下一步：AUTS 同步失败后的后续 challenge 当前会使用 Go 兼容分支，尚需 Rust 多轮状态接管；原生 PC/SC 阻塞任务在 HTTP 超时后仍可能运行，必须完成跨超时的物理操作互斥与取消边界验证，才能切换默认路径并删除 Go。没有执行真机，也没有生成 DMG。

### 1.46：读卡器超时任务的串行边界（2026-09-24）

- 基线：Rust 的六个鉴权/身份入口共用按 reader 隔离的阻塞锁。HTTP 超时不会停止已开始的 PC/SC 调用，后续请求会在此锁后排队，可能在原请求已过期后继续访问卡片。
- 目标：同一 reader 忙时在打开 PC/SC 前立即拒绝，绝不在 Rust reader 锁后排队；不同 reader 仍可独立操作。HTTP 超时对尚未开始的阻塞任务执行 abort，已运行的任务保留锁直至结束。
- 实现：复用已有 reader 锁表，六个入口统一使用 try_lock，映射既有 SharingViolation/reader_busy；鉴权、EAP-AKA、身份读取三个 HTTP 入口在超时分支取消排队任务。无新依赖、无额外轮询或 IPC。
- 测量：以线程和通道模拟请求离开后仍工作的读卡任务，验证两种 AKA 入口与身份读取均在 PC/SC I/O 前被拒绝、另一 reader 不被阻塞、原任务结束后锁可再次取得；reader 与 daemon 测试及 release 构建作为本轮门禁。
- 取舍：同设备并发请求改为显式忙错误，上层需按既有生命周期重试；避免过期任务积压的收益超过这个行为变化成本。此修复不声称可以中断操作系统正在执行的 PC/SC 调用，也不证明 Go eSIM 写卡与 Rust 的跨进程取消边界已完成。默认 EAP 路径和 UI 未调整，Rust 归属仍为 60%，真机门禁未推进。

### 1.47：身份读取的超时与错误契约（2026-09-24）

- 基线：Rust 身份端点工作超时为 15 秒，Go 却使用 7 秒通用 HTTP 超时；身份端点把设备忙/移除等均折叠为 HTTP 502，无法利用 1.46 新增的即时 busy 结果。
- 目标及测量：身份读取客户端上限改为 20 秒，调用者 context 更短时仍优先取消；身份与 AKA 使用同一 PC/SC 分类函数。用 Unix socket 故障注入检查 busy、absent、unavailable 三种错误能保持为 Go 既有类型，用 reader 单测验证两类操作错误码一致。
- 改动：Rust 身份端点输出 HTTP 503/reader_busy 或 HTTP 409/reader_card_absent、reader_unavailable；Go 接收后映射为既有设备错误。移除重复 PC/SC 错误匹配。无新依赖。
- 取舍与边界：收益是消除跨进程超时窗口错配并保留可恢复故障原因，成本局限于接口适配；未改变当前可选身份复核失败时保留已读取 Go 身份值的行为，未声称 UI 已切换为 Rust 唯一状态源。默认 EAP、多轮认证、跨进程写卡互斥门禁仍待完成；归属比例维持 60%。

### 1.48：Rust 接管同步失败后的 AKA 续接（2026-09-24）

- 基线：Rust 返回 AUTS 后 exchange 被消费，Go 后续挑战因无令牌而回到 Go AKA builder。目标是在普通 EAP-AKA 路径保留 Rust transcript 与后续挑战处理，避免跨轮切换协议实现。
- 实现：同步失败响应携带新的 `next_exchange_id`；Rust 保存原 transcript、原截止时间和剩余续接次数，绑定 reader 与 USIM/ISIM。最多两次续接，耗尽后显式返回 `eap_sync_retry_limit`；旧令牌立即失效。成功无续接令牌，错误响应与 generation 变更释放未使用令牌。
- 测量：Rust 测试验证原期限不延长、旧令牌不可复用、跨 reader/应用拒绝、重试上限；Go 状态机测试依次模拟 AUTS 与成功挑战，断言两轮均调用 native provider、Go CalculateAKA 调用为零，最终密钥写入并清除令牌。Rust daemon/VoWiFi 共 141 项测试、Go swu 与 macOS 桥回归作为本轮门禁。
- 取舍：沿用既有有界存储，仅增加续接元数据和响应字段，无新依赖；两次续接是本地防止无限鉴权的边界，不是运营商能力结论。Fast Re-auth 假名缓存仍由 Go 处理。此轮没有真实 SIM/运营商验证，默认运行路径仍受原开关控制，归属比例保持 60%，不删除未过真机门禁的 Go 实现。

### 1.49：鉴权拒绝响应的统一回收（2026-09-24）

- 基线：Go IKE 会话在响应 Identifier/类型校验失败或 provider 报错时，未统一释放响应携带的新 exchange；返回的 MSK/K_aut 即使未被会话接纳，也没有主动清零。
- 目标和测量：所有未采用的续接令牌均释放、所有未采用的密钥均清零；有效成功和同步续接保持原行为。故障测试构造错配 Identifier 与非零密钥，检查旧/新令牌各被释放一次、原密钥缓冲清零且会话未接纳密钥。
- 实现取舍：在已有 provider 调用边界集中 defer 清理，减少遗漏分支，无新服务或依赖。Go swu race 测试作为状态与共享缓冲回归，macOS 桥编译测试验证接口。
- 下一步主线：Rust `IkeAuthSession::exchange` 目前只负责一轮加密收发，`Ikev2EapConversation` 负责独立 EAP 应答；需要把 Identity→Challenge/AUTS→EAP Success 接入同一个有界异步循环，再实现最终 AUTH/Child SA。现有可选 HTTP 接缝尚不等于 Rust 完整运行时，归属仍为 60%。

### 1.50：加密 IKE_AUTH 与异步 EAP 循环串联（2026-09-24）

- 基线：Rust IKE_AUTH 仅提供单轮加密收发，EAP 身份/AKA 构包独立运行；实际多轮会话由 Go 调度。目标是把这些能力串为可消费的 Rust 多轮驱动，复用原传输、重传及 SIM 校验，不增加 IPC 或新依赖。
- 实现：新增 `IkeAuthSession::authenticate_eap`，接受异步 SIM provider，执行 Identity/Challenge/AUTS/EAP Success；上限 16 轮、120 秒、两次同步失败续接，每轮沿用三次相同密文重传。会话按值移入异步操作，取消或错误时无法继续复用半完成会话，临时密钥使用清零容器。重复 Identity 清除旧密钥，Success 必须匹配最近响应 Identifier 且已通过 AKA MAC 校验。
- 测量：本机 UDP 对端用真实 AES-GCM 解密并检查递增 message ID、Identity NAI 与 AKA 响应，再发送加密 EAP Success。固定 Challenge 的 MAC 由 Go FIPS186-2/HMAC 实现生成并验证黄金字节，Rust 使用同一报文完成鉴权；提前 Success 场景验证不会访问 SIM 或建立假在线状态。测试为注入 SIM 结果，不涉及真实卡片。
- 验收边界：返回 MSK/K_aut 和最后的 IKE 响应供最终 AUTH/Child SA 消费，状态仍为 Authenticating。此 API 尚未接入应用生产启动路径；SIM 通知/快速重认证和真实运营商握手仍待处理。未完成最终 IKE AUTH、Child SA、ESP/IMS 接管，不能把 EAP 测试通过当成 VoWiFi 上线。归属比例仍为 60%，本轮不生成 DMG。

### 1.51：最终 EAP AUTH 计算与身份一致性修复（2026-09-24）

- 基线：Go 初始 IKE_AUTH 允许 IKELocalIdentifier/FastReauthID，最终 AUTH 却重新使用 IMSI 派生 NAI，可能签入不同的 IDi；Rust 尚无 EAP MSK→最终 AUTH 的计算入口。
- 目标与测量：初始 IDi 按实际发送内容保存，最终 AUTH 使用同一 payload body；缺少原始身份即拒绝。测试覆盖普通、自定义与快速身份，发送后修改配置，仍必须匹配初始 IDi 的独立 HMAC 计算。
- 实现：Go 修复身份重建；Rust 增加 `compute_eap_auth`，复用现有 PRF 支持和清零容器，计算 prf(prf(MSK, Key Pad for IKEv2), 原始报文|对端 Nonce|prf(SK_p, ID body))，限制输入大小并拒绝缺失密钥、原始报文或身份。双方以同一 SHA256 PRF 向量逐字节对照。无新依赖。
- 边界：此轮只提供最终 AUTH 计算，未完成 Rust SA_INIT 原始报文/IDi 所有权接线、对端 AUTH 验证或 Child SA 建立；真实运营商门禁未运行。Go 兼容路径的身份一致性修复已进入源码，Rust 归属比例仍为 60%。

### 1.52：已验证系统 SOCKS 端点交接（2026-09-25）

- 基线：Rust 在空代理输入时已经能发现 macOS/Surge SOCKS 并完成 SOCKS5 UDP ASSOCIATE；但 `/api/v2/vowifi/select-transport` 只返回模式，Go 在收到 `socks5-udp-domain` 后仍使用启动前的空字符串，错误拒绝“未配置代理地址”。这会在 Surge 配置重载或两次自动发现瞬时不一致时阻断 VoWiFi 启动。
- 目标和测量：仅当本轮 Rust probe 成功、模式为 `socks5-udp-domain` 时，向 Go 返回已解析的 `proxy_endpoint` 与来源；Go 必须使用该端点创建 UDP relay。以本地 Unix socket fixture 测量空输入 + Rust 自动发现端点可启动工厂，端点缺失、畸形或来源缺失均 fail-closed；fake-IP、utun、direct 响应不得携带该端点。
- 实现：Rust 将已握手的 `SocketAddr` 限定输出给 SOCKS 模式，Go 解码并复用该地址，不再二次系统代理发现或 DNS 解析。系统 HTTP/HTTPS 代理仍不被猜测为 SOCKS；ePDG 仍作为 SOCKS ATYP=DOMAIN 交给代理，fake-IP/utun 和已验证直连策略均不变。
- 取舍与边界：改动是单一控制面响应字段、兼容传输绑定和三组本地动态 fixture，无新 socket、DNS 路径或后台任务，收益超过维护成本。它只修复 Go/Rust 自动代理交接，不等同于 Rust 接管 IKE/ESP/IMS，也未运行设备、运营商或真实网络测试；生产所有权仍为 9/15（60%）。

### 1.53：被叫最终 SIP 响应与 CANCEL 原子边界（2026-09-25）

- 基线：被叫接听后若 IMS 边缘重传同一 INVITE，旧逻辑会在 `connecting` 状态回复 `486 Busy Here`，使已接听或即将接听的呼叫被运营商终止。初始修复若直接把状态置为 `connecting`，又会让媒体分配期的 CANCEL 泄漏本地 socket，或在首个 `200 OK` 写入中把 CANCEL 误判为 `481`。
- 实现：入站事务保留同一 To-tag 与 SDP，并在 `incoming`/`connecting` 的同 Call-ID/CSeq 重传中重发 200；媒体先在 runtime 外创建，仅在原 INVITE 仍待处理时原子挂入。一个窄的 final-response mutex 串行化 200、CANCEL 与显式拒接：CANCEL 若先胜出返回 `200 CANCEL + 487 INVITE` 并释放新建媒体；若 200 已写出才到达，则按 SIP 事务规则返回 481。所有终态清除缓存与预留标记。
- 测量：故障注入覆盖 `INVITE → answer → retransmitted INVITE`（两个 200、相同 To-tag/SDP）、媒体打开时的匹配 CANCEL（无遗留 packet/audio socket，`100/180/200/487`）以及阻塞首个 200 写入时的 CANCEL（不在瞬时状态中提前返回 481）。Go imscore 常规与 race 定向测试均通过。
- 边界：这是 Go 兼容 IMS 被叫事务的稳定性修复，不是 Rust 接管实时 SIP/媒体；未以离线测试声称 CTExcel/Giffgaff 被叫已经完成运营商验收。生产所有权仍为 9/15（60%）。

### 1.54：传真创建请求的幂等恢复（2026-09-25）

- 基线：Mac 经受限 SSH 发送传真创建请求后，若 NAS 已落库但返回响应丢失，用户重试会产生第二个任务并可能重复 Originate。
- 实现：每个传真会话生成稳定 `fax-job:<session-id>` 键；仅对结果可能未知的连接/协议/响应超时重试一次并复用同一键。NAS 用 SQLite `BEGIN IMMEDIATE` 原子查找或新建，重复请求返回原 job_id，只有插入者启动一次 Originate。
- 测量：本地 HTTP 故障注入模拟首次已提交、客户端丢弃响应后重试，断言数据库仅一个任务且仅一次 Originate；NAS 单测 13 项、Python 编译检查和 Rust daemon 133 项均通过。
- 边界：不更改 SSH 731、NAS 实例、SIP 配置、页面或真实传真线路；这是现有本地传真路径的可靠性修复，不能替代真实收发验收，也不增加 Rust 运行时 owner，归属比例仍为 60%。

### 1.55：BLE 快照与自动重连状态边界（2026-09-25）

- 基线：Rust 仪表盘快照调用私有 `reader/reconcile` 时，已选 BLE 读卡器会先在 `observePresence` 查询 Swift bridge 状态，再无条件查询一次 `listConnectedBLEReader`。Swift bridge 串行处理 GATT/APDU；第二个 status 会排到卡操作后，既增加超时和主线程压力，也不改变同一轮观察结论。另有两处时序缺口：Rust 上游超时为 2 秒，Go 却用脱离请求的 4 秒 background probe 并持有 reader mutex；而 Swift 自动重连的 `connecting`/`discovering`/`reconnecting` 状态会回空 reader，被热插拔监控在两轮后误判为物理拔出。
- 目标和测量：已选且稳定的 BLE 读卡器每次 reconcile 恰好一次 bridge `status`；握手宽限或 APDU 忙窗口为零次；未选读卡器或已选 USB 时仍恰好一次查询以发现独立 BLE。探测必须继承请求 deadline、不得跨 bridge I/O 持有 reader mutex、且在同一 `authMu` 闸门内完成，避免状态探测排到新 APDU 后。用本地 Unix bridge fake 注入阻塞 status、请求取消、断开期间的迟到响应、状态计数和连续 reconnect/disconnected 轮询，分别验证锁不阻塞、无旧 generation 发布、自动重连不停止 VoWiFi、明确断开仍两次确认后回收。
- 实现与取舍：观察结果携带同一次 bridge 返回的 BLE 身份，`readerReconcile` 复用它填充 inventory，不发第二个 bridge 请求；bridge `Status` 的 transitional state 映射为可恢复错误，只有 `idle/scanning/disconnected/failed` 的空身份才是确定缺失。没有连接、卡管理或 APDU 协议改动；没有选择 Rust 直连 BLE，因为 Rust 目前只安全覆盖 PC/SC，缺少跨进程 reader+SIM 租约。该稳定性修复不构成 Rust 独占 VoWiFi owner，归属仍为 9/15（60%），ESTK 真机、AKA 和 VoWiFi 验收仍待后续门禁。

### 1.56：初始被叫响应失败与业务就绪展示（2026-09-25）

- 基线：实时 Go IMS 收到新 INVITE 时忽略初始 `100 Trying` 和 `180 Ringing` 的 responder 写入错误。若 Port-S 短连接已关闭，代码仍会保留 `incoming`、远端 SDP 与 responder；没有后续 CANCEL/BYE 时会形成幽灵来电并拒绝下一通。界面同时可能仅依据 `imsState=registered` 显示绿色，尽管 `callReady=false`，造成“注册成功但业务通道不可用”的假在线展示。
- 目标和测量：初始 100 或 180 的写入失败必须只终结对应呼叫、统一回收 incoming/media/远端 SDP，并允许不同 Call-ID 的下一通 INVITE 正常进入 `[100,180]`；不得把已注册 IMS 会话整体判死。以两个 responder 注入 `net.ErrClosed`，检查 failed 状态、资源为空和下一事务可进入 incoming；现有 imscore 常规及 race 被叫事务测试为门禁。菜单、活动和通话检查器的绿色“就绪”以 `callReady` 为准；只注册时使用中性“业务恢复中”文案，保留现有关闭开关与 IMS 轮询触发语义。
- 边界：这修复 Go 兼容实时被叫事务与展示一致性，不迁移 SIP/媒体 owner，也不将离线 Port-S 写失败测试等同于 CTExcel/Giffgaff 运营商来电验收。生产 owner 保持 9/15（60%）；真实 SIM 被叫、PCMA 媒体和长连接门禁仍需后续真机循环。

### 1.57：系统代理多地址 SOCKS 探测（2026-09-25）

- 基线：系统代理允许 `localhost:端口`，但 Rust SOCKS probe 只使用 DNS 返回的第一个地址。macOS 常先给 `::1`；当 Surge/Clash 仅监听 `127.0.0.1` 时，会把可用 IPv4 SOCKS5 UDP ASSOCIATE 误判为失败，继而落入既有 fake-IP/路由策略的拒绝或直连分支。
- 目标和测量：代理主机 DNS 与全部 SOCKS 候选共享单一 6 秒预算，去重并最多尝试八个候选地址；优先返回真实 UDP ASSOCIATE 成功的 endpoint，全部不支持 UDP 时保留解析顺序最早的 TCP-only 成功。Go 的 VoWiFi 启动调用为 Rust 结果保留 1 秒本机 IPC 余量，并以 7 秒有界 startup context 防止 UI 永久停留在“启动中”。IPv6 失败 + IPv4 成功、TCP-only fallback 与字面地址/无端口拒绝均以本地 SOCKS fixture 注入验证。
- 边界：变更仅针对系统/显式 SOCKS 端点本身，ePDG 仍不被本地解析，SOCKS 数据报仍以 ePDG domain 交给代理，fake-IP、utun 与合法直连策略不变。未运行真实 Surge/Clash 或运营商网络；这是代理启动兼容性修复，不代表 Rust 已接管 IKE/ESP/IMS，归属比例保持 9/15（60%）。

### 1.58：被叫事务占位、有限响应写入与 BLE 操作代际隔离（2026-09-25）

- 基线：初始入站 INVITE 在发出 `100 Trying` 前尚未占用本地 call slot；若响应写入阻塞/失败，另一个本地外呼或重传可先覆盖状态，随后通用 `finish()` 会误清理新会话。重复 INVITE 的 `180 Ringing` 也可能与接听 `200 OK` 穿插，造成终态之后写临时响应。另一方面，BLE 卡操作只用全局布尔 `bleBusy`：断开重连后，旧操作 release 可将新读卡器的忙状态清掉。
- 实现：将初始 `100/180`、重传响应、CANCEL 和本地 call-slot 申请置于同一入站事务 mutex；只在完成首个事务判定后允许本地拨号占位。Port-S 响应写入增加 4 秒 deadline，故障连接不能无限持锁。定向并发测试覆盖“阻塞 100 后本地拨号必须等待、失败后仍保留新 call identity”与“阻塞重传 180 时 final 200 不得先发”。BLE 忙状态改为 active/busy generation 原子租约，release 仅可清理自己 generation；测试注入重连后旧 release，确认新读卡器仍保持 busy。
- 取舍与边界：没有改 SIM APDU、BLE GATT 协议、ePDG 或任何 carrier 网络行为；公共 `reader/list` 的一次性 bridge 断态和 PC/SC/BLE 并行枚举仍是独立 P2，须先做 generation-bound API 集成测试再改，避免以大范围锁重排换取表面优化。上述回归为本地故障注入，不代表运营商来电或多读卡器真机验收；生产 Rust owner 仍为 9/15（60%）。

### 1.59：BLE 扫描与已连接读卡器链路隔离（2026-09-25）

- 基线：打开“蓝牙读卡器”面板或点击重新扫描会无条件取消 `desiredID`/`connectedID`、清空 APDU 特征与自动重连任务，再把 bridge 状态发布为 `scanning`。后端将空 reader 作为确认拔卡，可能停止正在运行的 VoWiFi/IMS/SMS 会话；这也会让读卡器扫描本身变成用户不可见的断线操作。
- 实现：CoreBluetooth manager 增加独立 `isScanning`。当状态为 connecting/discovering/connected/reconnecting 时，扫描只合并发现列表并保持现有 reader ID、GATT 特征和 reconnect work；无活动链路时才清理已失败或已断开尝试并启动干净的扫描。显式 `disconnect()` 仍是唯一清除当前会话的用户操作；连接新设备仍会停止扫描后切换。
- 测量：macOS27 Debug app 构建通过；Go 定向 reader/BLE reconcile、hotplug 和 IMS incoming 测试通过。该验证只覆盖状态机与 bridge 契约，不包含 ESTK 物理重扫、运营商 IMS、短信或通话真机循环。
- 边界：不改 BLE APDU framing、AKA、eSIM Profile、ePDG/前置代理或 Rust VoWiFi owner；这是 Go/Swift 兼容链路稳定性修复，不能计入 Rust 实时 owner，归属仍为 9/15（60%）。

### 1.60：运营商模板 ePDG 地址兼容（2026-09-25）

- 基线：上游 `ios-carrier-profiles` 已在部分 bundle 使用 `ims.epdg.address`，但旧解析只取 `candidates` 或 `remote_certificate_hostname`。直接更新快照会让非标准 ePDG 静默退回拼接的 3GPP 域名；Rust native parser 对 legacy certificate hostname 的兼容也不完整。
- 实现：Go carrier expander 与 Rust native carrier parser 统一为 `用户 custom → ims.epdg.address → candidates[0].address → remote_certificate_hostname → 标准 3GPP FQDN`。只解析本地嵌入模板，不对 ePDG 作额外 DNS，也不按文件名猜测运营商；CTExcel/giffgaff 当前快照的标准 FQDN 保持不变。
- 测量：Go carrier 定向测试与 Rust `proletarius-vowifi` carrier 测试均通过，覆盖 direct address、candidate、legacy hostname、custom precedence、Verizon 风格非标准 hostname 及标准回退。
- 边界：本轮未更新上游 registry snapshot/revision，避免把 254 个模板变化和解析迁移混入同一回归面；未运行运营商真机 IKE/IMS 验证。解析兼容不改变 Rust 当前 Go-owned IKE/ESP/IMS 归属，生产 owner 仍为 9/15（60%）。

### 1.61：会话冻结的 USB 标准 EAP-AKA 子路径（2026-09-25）

- 基线：此前 Rust EAP Identity/AKA 接缝依赖进程级环境变量，无法把 Unix socket、读卡器、generation 与 ICCID/IMSI 绑定为同一会话；运行期切卡、重连或 socket 配置变更可能使后续请求落入不同 sidecar。原 Go 与 Rust 的 AKA 截止时间同为 35 秒，物理读卡刚完成时 Go 可能先超时；`authMu.Lock()` 也不能响应已停止会话。
- 实现：VoWiFi 启动时仅在显式 `PROLETARIUS_RUST_EAP_AKA=1` 试验许可、USB/PCSC、完整 ICCID/IMSI、非空绝对 socket 条件同时满足时冻结 `reader + generation + identity + socket`；普通发行默认仍由 Go 驱动 EAP transcript。许可仅在会话创建读取一次，运行期环境变更不能切换 transcript owner。既有 Go 路径下可选的 Rust PC/SC AKA helper 不在本轮改变范围，不能被误计为完整 Rust EAP 会话。标准 EAP Identity/AKA 使用该冻结能力；选择 Rust 后的错误以 `handled=true` 失败闭合，绝不再隐式进行第二次 Go AKA。Rust worker 截止为 30 秒，Go HTTP 留 35 秒响应余量；等待 Go 读卡认证锁改为可取消的 TryLock 轮询。Identity exchange 清理使用独立 2 秒有界 context 和冻结 socket。
- 保留：BLE、EAP-AKA'、Fast Re-auth、带 `AT_NEXT_REAUTH_ID` 的分支仍强制走既有 Go PC/SC 路径；Rust IKE/ESP、IMS、SMS、通话和 UI 未接管。旧的全局 socket 兼容包装暂保留至真实差分验收结束，防止在本轮同时扩大删除面。
- 测量：Go 覆盖 USB/BLE/身份/socket gate、冻结身份、native fail-closed、取消 Identity/AKA、环境变更后的 cleanup 与锁等待取消；另以 Unix fake 做一次冻结 Identity→标准 AKA 成功闭环，逐项核验 exchange ID、checkcode、reader/SIM binding、MSK/K_aut 与 socket 不随环境漂移，并验证运行期改变试验许可不改变已创建会话的 conversation owner。vendored `swu-go` 覆盖 Fast Re-auth 与 `AT_NEXT_REAUTH_ID` 回退。Go reader、IMS、carrier 全量包测试，以及 Rust workspace 的严格 warnings 测试均通过；macOS27 arm64 Debug 构建成功并完成 ad-hoc bundle 签名校验。
- 边界：未进行真实 USB SIM、ePDG、IMS、SMS 或通话测试，不能以该子路径把 `reader_vowifi` 标为 Rust 独占。生产所有权继续为 9/15（60%）；下一门禁是 CTExcel/giffgaff 各自以此 session 走完 IKE→IMS→SMS/语音并进行拔卡、切卡与 sidecar 重启故障注入。

### 1.62：系统代理优先、DNS 边界与可取消启动（2026-09-25）

- 基线：空代理时 Go 与 Rust 曾分别发现系统代理；系统 SOCKS 成功后 Rust 仍并行本地解析 ePDG，泄漏了本应交给 SOCKS `ATYP=DOMAIN` 的名称。Surge 已接管系统代理但增强模式未开启时，正常公网地址会被直连分支提前选中；启动还可能在持有全局会话锁时等待代理检测或 Surge CLI，使停止操作无法及时取消。
- 实现：Rust 是唯一自动发现者。它只在 `scutil` 的 loopback HTTP/HTTPS listener、Surge `system-proxy` 状态和有效 profile 的同地址族 HTTP listener 全部一致时标记 `surge-effective-profile`；普通 macOS SOCKS、显式代理、HTTP/PAC 诊断均保持独立。已验证的 SOCKS5 UDP ASSOCIATE 立即选择 domain relay，之后不再作本地 ePDG DNS 或路由查询。无可用 SOCKS 时才评估 fake/utun 或直连；直连只接受全局可路由 IPv4/IPv6，拒绝 private、loopback、link-local、fake、文档、组播和特殊过渡地址。Go 以可取消 startup reservation 在锁外完成选择；停止会取消 reservation 并阻止迟到发布。仅对 Rust 已确认的 Surge 自动来源允许一次、35 秒有界的增强模式恢复与重新选择；显式代理、Clash/mihomo/macOS SOCKS、HTTP/PAC 与所有来源不确定情形都不会修改 Surge。
- 测量：本地 fixture 覆盖 IPv4/IPv6 listener、地址族错配、HTTP/PAC、SOCKS 成功无本地 ePDG 解析、私有/特殊直连拒绝、Surge direct/rejected 的单次恢复、来源变化 fail-closed、启动取消和 generation 防迟到发布。Go reader 包和 race 测试、Rust `proletarius-vowifi` 与 `proletariusd` 定向测试通过。
- 边界：未读取、改动或真机验证 macOS System Proxy、Surge、Clash、读卡器或运营商网络。该改动只收紧当前 Go production path 的代理边界，`reader_vowifi` 的 IKE/ESP/IMS/SMS/通话 owner 仍是 Go，生产所有权仍为 9/15（60%）。

### 1.63：IMS 运行时真值与关闭竞态收敛（2026-09-25）

- 基线：REGISTER 成功后若 Port-S/安全传输启动失败，状态可能仍看似已注册；关闭与拨号、短信、传真或异步 RP-ACK/INVITE watcher 交错时，旧代码可读取已清空的 lifecycle context、runtime、call 或 fax 指针。并发 Start 也可能在 Close 后重新提交旧运行时。
- 实现：REGISTER 只有在认证 transport runtime 成功启动后才提交为可用；状态独立报告 `registered`、`reg_status`、`signaling_ready`、`transport_ready` 与 `incoming_ready`，底层失效时明确降级。每个 `Service` 使用 lifecycle ID 与 `startDone` 防止 Close 后复活；业务操作获得短生命周期 lease，Close 先拒绝新操作、取消、等待已获准操作，再原子拆离 runtime/call/fax/SWU/client。启动和关闭窗口中的 Port-S SIP 请求以明确 SIP 响应失败闭合，不留无响应事务。关闭后的 Service 是终止态；读卡器重连会创建新的 Service，避免跨代复用。
- 测量：覆盖 OPTIONS=0 的 runtime 启动失败、健康→传输失败状态降级、取消前 commit、lease 与 Close、终止态 Start、阻塞 REGISTER 与 Close 交错、来电/RP-ACK 异步路径；`imscore` 常规与 race 回归通过。
- 边界：Close 会等待已获准的调用方回调（例如长期阻塞的传真 callback 或 SIP responder）返回，而不是冒险释放正在使用的资源。未进行真实 IMS、来电、SMS 或运营商测试；这是当前 Go IMS 的稳定性修复，不改变 Rust 生产 owner 或 9/15（60%）计数。

### 上游参考复核（2026-09-24）

- `ios-carrier-profiles` 要求 home PLMN 保留 MNC 原始宽度、按受约束条件和 specificity 选择、拒绝最高分歧义、缺少 GID 时不猜 MVNO，并以本地缓存优先于在线数据。当前 Rust 使用内嵌版本化快照，不在每次启动联网；853 个 PLMN selector、683 个 bundle 与 Go 导出的 synthetic cases 做离线对照。此策略避免启动依赖网络及将 IMSI/ICCID/GID 放进 URL；代价是新运营商 bundle 随应用更新。本轮不增加动态下载，因为收益不足以抵消网络依赖、隐私和缓存一致性复杂度。
- `NekokoLPA2` README 确认其范围包括 BLE、USB CCID、多读卡器与 eSIM Profile 管理；这支持 CardService/transport 解耦，但其能力列表不能证明特定 ESTK 固件握手，也不证明 VoWiFi IKE/IMS 行为。仍以 reader 协议测试及后续 ESTK 实机配对/读卡验收证明硬件行为，不从 LPA 可用推导 VoWiFi 已可用。

### 门禁状态

| 门禁 | 当前状态 | 说明 |
| --- | --- | --- |
| Rust 协议单元测试 | 已通过 | IKE、IMS、SMS、PCMA、传真及代理策略均有覆盖 |
| Go/Rust 差分测试 | 部分通过 | 已覆盖 SIM/AKA、编解码、归档、租约刷新和重连退避；IKE/ESP 与实时 IMS 会话仍需扩展 |
| 取消、重连、拔卡故障注入 | 部分通过 | 已覆盖停止等待、重复启动保护、快照身份 fail-closed 隔离，以及仅对已选读卡器生效的两次确认、generation-safe 热拔清理；扫描/桥接错误不计为拔卡。仍需物理插拔与 Rust 实时传输接管后的复测 |
| Surge/Clash 前置代理与合法直连真机循环 | 部分通过 | 1.14 发布前验证链路已通过 Surge 空白代理自动接管、Rust AKA、IMS/SMS/call ready、2400 秒绑定续期调度及三轮故障恢复；Clash 与直连仍需完成同版本循环 |
| 读卡器 VoWiFi 默认 owner 切换 | 未开始 | 依赖以上门禁全部通过 |

仓库级门禁已接入本地脚本和 macOS 发布 CI；每次发布会同时执行 Rust
workspace 测试、Go vet 和 Go 兼容桥测试。CI 通过只代表代码级门禁通过，不能替代
读卡器插拔、Clash 前置代理和运营商 IMS 的真机循环。

### 迁移取舍记录

- `SOCKS5 UDP relay` 暂不作为独立跨进程组件迁移。Rust 已拥有代理探测、
  域名封装、传输选择和策略决策，但当前 `swu-go` 的 `Transport` 接口在 Go
  进程内消费数据包。只迁移 relay 会为每个 IKE、NAT-T 和 ESP 包增加一层
  Unix IPC、缓冲队列、取消同步与进程重启恢复，运行成本和新故障面高于删除
  现有 Go 适配器的收益。该适配器与 IKE/ESP 实时 owner 一并切换，不建立
  过渡性的双数据面。
- 跨进程读卡器 lease 暂不单独落地。Go 的 `authMu` 只能串行 Go 内部 APDU
  操作，不是 Rust 直连 PC/SC 或 BLE 的安全租约；而 native VoWiFi 入口当前
  仍明确 fail-closed。单独引入 acquire/status/release 协议会增加约一套
  生命周期、TTL 和 BLE 兼容表面，却不改善当前生产路径。待 Rust 生命周期
  actor、generation 校验和真正 native session 同轮接入时再实施，避免形成
  无消费者的伪租约或把未验收路径计入 60%。
- 多读卡器不以 `SessionRegistry` 的 map 形状冒充完成。当前 physical reader
  manager、VoWiFi admission、SMS/call archive 和 BLE bridge 都仍是单实例；
  先将 Telephony API/归档收束到 `reader_id + sim_id + generation` 的
  fail-closed session reference，之后才评估 per-reader manager 与 keyed
  lifecycle actor。该顺序能先阻断串卡/串记录风险，又不会在未验证 APDU Port
  与多 BLE 外设并发前开放第二条 IMS 会话。
- 1.15 修复传真 Keychain 后台读取的 Swift 并发捕获，并使用禁止交互的
  `LAContext` 替代已弃用的 Security UI 查询键。收益是避免启动阶段阻塞及
  Swift 6 编译错误，改动不触及传真协议或既有 UI，达到实施阈值。
- 1.15 将同一 Rust 传真 Agent 客户端的 SSH 请求串行化到共享异步门禁。
  启动时的健康、能力与收件箱请求不再并发争用同一 OpenSSH ControlPath，
  避免 `ControlSocket already exists` 后退化成多条独立 SSH 连接。六路并发
  真实 NAS 健康检查已通过；SSH 731、forced-command 与 SIP 参数均未改变。
- 1.16 修复 IMS OPTIONS 保活的临时响应误判。基线：原逻辑收到首个 SIP
  响应即标记本轮保活成功，`100 Trying` 后若无最终响应仍可能让 UI 维持
  在线。目标：只在 12 秒内收到 `>=200` 最终响应时确认链路有效；仅有
  临时响应、响应流关闭或超时均触发已有断链重连路径。用定向测试注入
  `100→200`、`100→超时` 与通道关闭三种情况测量。此改动只调整确认
  条件，不增加网络请求，收益超过一个小函数和测试的维护成本；尚未据此
  宣称 CTExcel 被叫真机问题解决。
- 1.17 修复 Rust IKE 影子会话的重传窗口。基线：上一轮响应仅凭交换类型与
  Message ID 即可被判为重复，未先检查 SPI 与响应标志；下一轮请求正在等待
  时，上一轮合法重传又被拒绝。目标：伪造/错误 SPI 的重复响应拒绝率 100%，
  合法迟到重传可忽略且不改变待处理请求。单元测试注入两类 SPI 错误、错误
  标志与迟到重传，`proletarius-ike` 测试及严格 Clippy 通过。改动仅限尚未
  默认接管硬件的 Rust 协议状态机，收益高于小幅判定重排的成本；实时 IKE
  owner 未因此切换，仍需完整 Go/Rust 差分及真机门禁。
- 1.18 修复 Rust 音频 SDP 的媒体地址回退差异。基线：存在有效会话级 `c=`
  时，若音频媒体级 `c=` 是主机名或无效地址，Rust 会退回会话地址，而现行
  Go 业务路径会拒绝。目标：无效媒体地址拒绝率 100%，Go/Rust 在该故障
  注入样本上同为拒绝；定向单元测试和跨语言 SDP 差分通过。此修正避免
  将 RTP 错发到会话级地址，成本限于一处分支和样本，符合收益阈值。
  同轮硬件探测确认一台 ESTKme-RED 已接入，Rust PC/SC 共享连接、事务、
  只读 SELECT MF 和关闭均通过；没有发现本机 127.0.0.1:7897 监听，故
  未启动网络真机循环，不能把该读卡结果视为 VoWiFi/通话通过。
- 1.19 修复仍负责真机业务的 Go IMS 被叫事务边界。基线：相同 INVITE
  重传被忙线分支回复 486；不匹配 Call-ID 的 CANCEL/BYE 和 ACK 可影响
  当前呼叫。目标：同 Call-ID+CSeq 重传保持原 To-tag 并回复 180，另一
  INVITE 事务仍为 486；不匹配 CANCEL/BYE 回复 481，ACK 不触发媒体；
  匹配 CANCEL 才结束原呼叫。用单元故障注入逐项验证，收益高于这些窄
  条件分支的维护成本。此处不改变 Rust/Go owner，也不把测试通过当作
  CTExcel 实际来电已恢复；需代理可用时再做运营商被叫真机循环。
- 1.20 将 Rust 前置代理选择中的 SOCKS5 能力探测与 DNS→路由验证
  并行，并将每个 ePDG 地址的路由查询并行且按原 DNS 顺序汇总。基线：Go
  本机调用窗口为 7 秒，代理探测最多 6 秒；原实现随后逐个路由查询，
  每个最多 3 秒，多个地址时可越过调用窗口。目标：正常超时上界接近
  `max(代理探测 6 秒, DNS 3 秒 + 路由 3 秒)`，不因地址数线性增长；
  故障注入使用同步屏障证明查询真正并行，故意让首地址慢完成仍验证选择
  顺序不变。此优化不改变 DNS/代理策略、不增加绕过前置代理的路径，
  减少超时的收益高于局部异步汇总的维护成本。未把代码级通过视为
  Surge/Clash 真机门禁通过。
- 1.21 建立 Rust 单 ePDG 端口数据报通道，作为未来同进程 IKE/NAT-T/ESP
  的传输接口，不新增 Go/Rust 逐包 IPC。基线：Rust 只具备代理探测与
  SOCKS5 UDP relay，尚无统一的按已选模式收发入口。目标：透明模式使用
  系统路由的通配地址 UDP socket，SOCKS5 模式以 ATYP=DOMAIN 向代理
  发送 ePDG 主机名；缺失代理时不得降级直连，回包端口错误须显式拒绝。
  本地 UDP 回环和模拟 SOCKS5 握手/收发/错误端口测试通过。由于收益是
  消除未来重复的数据面接线且不会触碰现有 Go owner，改动成本可接受；
  尚无 IKE 加密、ESP 状态或 IMS 会话接入，不能宣称实时 Rust 传输已
  完成，更不能删除 Go 数据面。
  同轮 Surge 真机验证：macOS 系统代理为 127.0.0.1:6152，Surge 增强模式
  已启用，ePDG 路由落在 utun4。通过隔离启动的 1.21 后端连接一台
  ESTKme-RED，自动选择 `enhanced-mode-utun`，完成 SIM AKA、IKE/ESP、
  IMS REGISTER；读卡器 SMS 向 888 发送一次 BAL 返回成功，888 外呼一次
  到达 active/media_ready，实际协商 AMR。测试结束已挂断、关闭 VoWiFi
  并停止隔离后端。此结果不证明短信被运营商最终投递、Mac 双向音频质量、
  CTExcel 被叫可达，也不等同于 Rust 实时 IKE/IMS 已接管。
- 1.22 对齐 Go 真机通道的 IKE 500→NAT-T 4500 原位切换。基线：Rust
  每个端口新建通道，SOCKS5 会重新 UDP ASSOCIATE，代理侧映射可能变化；
  Go 当前在同一关联中更新目标端口。目标：切换后本地 UDP 端口或 SOCKS5
  关联不变、ePDG 域名仍由代理解析，回包按新端口验证。透明 UDP 与模拟
  SOCKS5 切换测试均通过；改动仅限尚未接管实时业务的 Rust 传输接口，
  不干扰已验证的 Go/Surge 真机基线。收益是减少 NAT-T 切换的代理状态
  不确定性，超过单个接口与测试的维护成本。实时 owner 切换仍受差分与
  真机门禁约束。
- 1.23 为 Rust ePDG 通道增加明确的 IKE、ESP 与 NAT-T 保活发送入口。
  基线：新通道只有原始 `send`，若将 IKE 会话直接接上，4500 端口缺少
  4 字节非 ESP 标记，运营商会把信令误判为 ESP。目标：500 端口 IKE
  原样发送，4500 端口 IKE 加标记，ESP 不加标记，保活为单字节 `ff`；
  ESP/保活在非 4500 端口明确拒绝。用 SOCKS5 模拟关联检查域名及
  500→4500 后三种载荷，透明通道与纯封装测试验证 500/4500 分支。
  此接口避免未来实时接线时复制错误封装，收益高于小范围的新增测试
  和接口成本；Go 真机 owner 不变，尚不能视为真实运营商媒体通过。
- 1.24 补齐 Rust ePDG NAT-T 接收分流。基线：通道只返回原始 UDP
  字节，未来接线时容易把保活 `ff` 或 ESP 送入 IKE 解码器。目标：500
  端口只接收通过 IKE 头校验的信令；4500 端口按非 ESP 标记、有效 ESP
  头与单字节保活分为三类，畸形包显式拒绝。纯分类故障注入覆盖短 ESP、
  缺 IKE 头和错误端口；模拟 SOCKS5 UDP 关联实测三类报文分流。收益是
  减少后续实时 owner 接线时的媒体串流风险，超过局部分类器成本；
  Go 当前真机实现仍不受影响，Rust 接管与真实语音仍待门禁验证。
- 1.25 处理 NAT-T 切换时代理迟到包的差分风险。基线：Rust 收到与当前
  ePDG 端口不符的 SOCKS5 UDP 回包立即报错，可能把正常迟到的 :500 包
  误判为断链；Go 真机路径会按回包端口改写目标，存在从 :4500 回退的
  相反风险。目标：Rust 保持当前端口与 SOCKS5 关联，只丢弃旧端口回包，
  继续等待有效回包；TCP 控制通道关闭与真正收包错误仍传播。模拟代理
  在切换前注入 :4500、切换后注入 :500，均验证后续正确回包可达。
  仅改未接管真机的 Rust 路径，收益是避免错误重连或端口回退，超过
  局部循环与两组注入测试的成本；Go 真机行为暂不改变。
- 1.26 将 Rust IKE 头状态追踪与 ePDG UDP 通道连接为仅覆盖 SA_INIT 的
  请求运行器。基线：两者各自有测试，但没有一次真实 socket 的请求、
  响应、重传与超时闭环。目标：同一请求字节按固定次数重发，每次有独立
  截止时间；只接受匹配 SPI/消息 ID 的响应，错误 SPI 立即拒绝。模拟
  ePDG 的正常、超时重传及错误 SPI 注入均通过。该接口比重复的独立
  parser/socket 测试更接近实时接管且不增加跨进程 IPC，收益超过维护成本。
  IKE 提案/DH、EAP-AKA、密钥派生、Child SA、ESP 数据面仍未实现，
  因此尚不能切换默认 owner，也不宣称 Rust 真机注册成功。
- 1.27 迁移 IKE 通用载荷链的首类型、Next Payload、长度和 Critical 位
  编解码，并接到 Rust SA_INIT 请求运行器。基线：请求只能传不透明字节，
  无法安全组织 SA/KE/Nonce/通知载荷。目标：与现行 Go `IKEPacket.Encode`
  在相同 SA/KE/Nonce 样本上输出逐字节一致；短头、短主体、错误长度和
  尾随字节明确拒绝。Rust/Go 共享黄金字节样本与模拟 ePDG 请求均通过。
  这是后续移植真实提案和 DH 的必要小模块，收益超过约一处编解码与
  两侧测试的成本；加密与实网 owner 仍由 Go 负责，未做真机宣称。
- 1.28 修正 Rust SA_INIT 响应的完成判定与 COOKIE 交互。基线：只匹配
  IKE 头/SPI 即返回成功，缺 SA/KE/Nonce 的响应也可能被接受；零 Responder
  SPI 的 COOKIE 被先行拒绝。目标：三类强制载荷存在且非空；COOKIE、
  NO_PROPOSAL_CHOSEN 显式报告；COOKIE 长度限 1–64 字节，原 SPI 与
  SA/KE/Nonce 不变、COOKIE 放首载荷，只自动重试一次。模拟 ePDG 覆盖
  正常、缺载荷、COOKIE 零 SPI、错误 SPI、提案拒绝和有界重试。
  [RFC 7296 §2.6](https://www.rfc-editor.org/rfc/rfc7296) 明确允许 COOKIE
  响应的零 Responder SPI，且要求重试保持原载荷并把 COOKIE 放在首位。
  收益是防止未建 SA 却误报成功、并满足运营商抗 DoS 挑战；仍未实现
  提案/DH/密钥派生，不切换默认真机 owner。
- 1.29 将 Rust SA_INIT 成功响应整理成可供后续密钥派生消费的结构：
  Responder SPI、SA 原始载荷、KE DH 组及公钥、Responder Nonce 与完整
  原始响应。基线：返回的仅是不透明 IKE 字节，未核对请求 KE 组和响应
  KE 组一致，可能把无效协商带入后续阶段。目标：请求/响应 KE 格式与
  保留位有效、DH 组非零且一致；模拟 ePDG 的一致/不一致组与畸形 KE
  测试通过。受益是为 DH 与 KDF 迁移提供明确输入并提前拒绝错误参数，
  超过小范围结构化解析的维护成本。SA 提案算法本身尚未解析或比对，
  因此仍不能据此宣称 Rust SA_INIT 真机可用。
- 1.30 将 Rust IKE SA 提案解析接到 SA_INIT 响应校验。基线：Rust 已
  提取 KE/Nonce，但对 SA 只保留原始字节，不能判断 ePDG 是否选中了
  本次报价里的算法。目标：严格解析 Proposal、Transform 与 TV/TLV
  属性长度；选中的单一 IKE Proposal 必须匹配本次报价的编号、协议和
  完整 Transform/属性，且包含加密、PRF、DH；AEAD 不得另带完整性
  Transform，非 AEAD 必须有完整性 Transform，DH 要与 KE 组一致。
  Go AES-GCM/PRF/DH 黄金字节、Rust 模拟 ePDG 与未报价算法/畸形长度
  注入通过。收益是阻止不受支持的算法进入密钥派生，超过解析器与定向
  测试的维护成本；实际 DH/KDF、EAP-AKA 和 ESP 尚未切到 Rust。
- 1.31 恢复模块页 eSIM 管理入口，复用既有模块 AT/eUICC 后端提供独立的
  切卡、下载和删除操作，并在切卡重启中断响应时查询操作状态以避免盲目
  重试。模块 Wi-Fi Calling 的旧前端原先指向已删除的 `/api/vowifi` 路由；
  现由 Go 硬件桥按需打开 DJI QMI IMS 通道，设置 VoiceWiFi/呼叫偏好，
  仅在 QMI 报告真实 WLAN/IWLAN IMS 注册时显示激活。模块固件的 ePDG
  由其自身发现；不接受无法生效的自定义 ePDG 参数，也不修改读卡器
  VoWiFi 路径。基线是按钮必然返回 404、eSIM 管理无入口；目标是接口
  可达、未接设备显式不可用、WWAN 注册不误报为 VoWiFi，三项定向测试
  及 macOS 构建通过。当前未接入 DJI 模块，固件是否具备 Wi-Fi 承载与
  真机呼入/短信能力未验证；本改动不构成模块 VoWiFi 可用性承诺。

### 1.64：传真恢复与入站 SIP 半关闭收敛（2026-09-25）

- 基线：Mac 进程重启会丢失仅存于内存的 NAS 传真任务；SSH/Agent 暂时
  不可达曾可能被误写为本地失败。另一方面，Port-S 入站 TCP 在携带
  INVITE 的同次 `Read` 返回 EOF 时会过早关闭写侧，使用户稍后接听时
  无法发送最终 `200 OK`。
- 实施：传真在文档上传后写入单条原子恢复记录，创建任务前保存
  `CreateReady`，受理后保存 `Accepted + job_id`；重启时复用原
  idempotency key，已受理任务仅查询/轮询。仅 NAS 明确终态才按
  `archive.add_once → clear journal` 完成；归档或清理失败每 15 秒重试，
  缺失或错配 journal 均失败关闭。Port-S 对半关闭 INVITE 保留写侧租约，
  至接听、拒接、CANCEL、服务关闭或 90 秒有界决策超时才释放。
- 验证：Rust `proletariusd` 154 项（包含传真恢复、并发、终态和临时
  Agent 故障注入）与 Go Port-S 定向 race 测试通过；未连接 NAS、SSH、
  读卡器、Surge/Clash 或运营商网络。该轮是当前 Go 兼容实时路径的
  稳定性修复，不改变 Rust 生产 owner，生产所有权仍为 9/15（60%）。
