# 项目架构与开发笔记

> 当前导航与功能口径对应 Switch 2.80/build280。侧栏为“活动、模块、读卡器、N待、短信中心、通话、调试与诊断”；“关于与更新”已移除。最新实现、候选取舍及未验收范围见 [`optimization/2026-10-03-protocol-hardening-execution.md`](optimization/2026-10-03-protocol-hardening-execution.md)。历史内容只在与当前源码一致时作为说明，不能作为新指令。

## 架构总览

```
Switch.app
├── Contents/MacOS/Switch      # SwiftUI 前端（进程宿主）
└── Contents/Resources/backend/
    ├── switchd-rs               # Rust 网关、原生状态/归档/编解码
    ├── switchd                  # 尚未迁移的硬件、VoWiFi/IMS 兼容运行时
    └── libusb-1.0.0.dylib            # 随包 USB 库（@loader_path 链接）
```

- **前端**：SwiftUI（`app/Sources/`）
- **通信**：Unix domain socket（`~/Library/Application Support/Switch/switch.sock`），Swift 侧自定义 `URLProtocol` 以 `http+unix://` 协议走 URLSession
- **后端**：Rust 网关（`rust/`）是应用唯一公开 socket；其功能归属门禁仅将已完成迁移的路由留在 Rust，仍需硬件会话的请求转发到受监督的 Go 兼容进程（`backend/`）
- **构建**：`xcodebuild`（支持 macOS 27 SDK 的 Xcode），Go/Rust 后端由工程的 post-build 脚本随 Xcode 构建一并编译嵌入；具体工具链随源码快照记录，CI 见 `.github/workflows/release.yml`

## 关键源码文件

| 文件 | 职责 |
| --- | --- |
| `app/Sources/BackendProcess.swift` | 子进程管理（自动启动/停止、socket 路径） |
| `app/Sources/ProletariusApp.swift` | SwiftUI Window/MenuBarExtra 生命周期：原生静默启动、信号与实时流量 |
| `app/Sources/DeviceSnapshotStore.swift` | 带 revision 的设备聚合状态源；拒绝旧实例和旧请求响应 |
| `app/Sources/DashboardStore.swift` | 页面协调器；组合模块、读卡器、通话、音频与网络子状态源 |
| `app/Sources/UnixSocketURLProtocol.swift` | http+unix:// URLProtocol（POSIX socket + HTTP/1.1 解析，支持 Content-Length/chunked） |
| `app/Sources/APIClient.swift` | URLSession 封装；dateEncodingStrategy = .iso8601（Go RFC3339） |
| `app/Sources/AudioBridge.swift` | CoreAudio IOProc 音频桥（重采样、输入输出设备与 AirPods 路由） |
| `app/Sources/Views/HomeView.swift` | 按 scope 呈现独立模块页与读卡器页；复用状态源，但不混用设备卡片或 VoWiFi 操作 |
| `app/Sources/Views/SMSView.swift` | 短信页面协调、搜索、选择与删除状态 |
| `app/Sources/Views/SMSConversationViews.swift` | 会话列表、聊天气泡、详情、清理与发送界面 |
| `app/Sources/Views/SMSCenterViews.swift` | 短信中心统一会话列表和对话界面 |
| `app/Sources/SMSForwardingStore.swift` | Telegram Bot / Bark 转发设置、钥匙串凭据和本机转发队列协调 |
| `rust/crates/proletarius-forwarding/` / `rust/crates/proletariusd/src/sms_forwarding.rs` | 短信转发的 SWF1 持久队列、provider 投递、退避重试与去重；Go 仅保留测试用 flock 互操作 peer |
| `app/Sources/MultiStandbyStore.swift` | 多 Profile 状态、独立短信、幂等发送与通知游标；短信统一呈现在短信中心 |
| `app/Sources/Views/MultiStandbyView.swift` | 莫比乌斯环 N 待工作区；只管理 VoWiFi 短信会话，不接入语音 |
| `backend/cmd/proletarius-macos/multistandby_card.go` | FIFO 卡片许可、物理身份确认、切卡恢复和私有 Go AKA |
| `backend/cmd/proletarius-macos/multistandby_manager.go` | 多会话生命周期、重认证代次、恢复原 Profile 与预留释放 |
| `backend/cmd/proletarius-macos/multistandby_runtime.go` | 计划续期/故障退避、脱敏运行事件与有覆盖率的在线/未知报告 |
| `app/Sources/Views/ReaderCardView.swift` | 读卡器、eSIM Profile 与 VoWiFi 操作入口 |
| `app/Sources/Views/DiagnosticsView.swift` | 网络诊断（弹窗）+ AT 调试 |
| `app/Sources/SafeDiagnosticSummary.swift` | 后端离线时也可导出的白名单状态摘要 |
| `backend/cmd/proletarius-macos/network_diagnostic.go` | 有预算的顺序诊断、取消等待与部分结果 |
| `backend/cmd/proletarius-macos/usbat_darwin.go` | libusb AT 桥（兼容 2ca3:4006 与 2c7c:0125） |
| `backend/cmd/proletarius-macos/main.go` | HTTP API + 短信归档 + 通话控制 |

## 关键设计决策

### 1. 设备短信与本机归档
- **SIM 卡（SM）** 与 **模块（ME）** 存储：硬件源头，短信保留在这里（默认）
- **内存缓存**：后端进程内，最多 500 条
- **本机归档**（接管模式开启、IMS/Modem 接收或发送记录）：`sms-archive.json`，最多 1000 条、8 MiB；接收确认前必须持久保存
- 短信列表 = 内存 + 归档合并去重（按 sender+content+timestamp 的 key）
- 单条删除按 存储+索引（SM/ME）或 sender+content+时间窗口（本机）
- 清空支持多选：SIM / 模块 / 本机（`POST /api/sms/clear`）

### 2. 短信接管模式（`smsAdopt`）
- 开启后：USB 轮询短信先持久化到本机，再发布；不自动整库清空 SM/ME，避免删除快照后新到达的短信，保留手动清理
- 状态持久化到 `sms-adopt.json`，**以后端为准**（app 启动拉取真实状态）
- 发送记录（direction=out）**总是**归档，不依赖开关

2.78 的 IMS 回调传播归档失败，Modem checked 回调只在保存确认后允许 ME 单片精确清理，并复核存储、相同 PDU 与观测 epoch。SM/未知存储、解码失败、未完成片及全部长短信片保留。长短信使用一次完整当前存储快照，先检查全组冲突，不跨轮缓存分片；受损历史仅余互补片时的 reference 归属仍不可证明。设备容量需要人工管理。

归档写入拒绝超限文件或会截断本次输入的容量变化，失败恢复确认前缓存；rename 后目录同步失败返回失败，重复接收重试目录同步，不提前 ACK。已提交的设备短信若归档失败返回不可自动重试的 `sms_send_archive_outcome_unknown`，保持草稿与查询提示。普通 reader-IMS 提交前校验两次同一观测身份/代次，不能等同于可信订阅租约；设备持久发件账本见下方 2.79 契约。

N 待会话短信独立于读卡器 SIM/模块存储，使用各自 Profile 会话与消息 ID；界面统一进入短信中心。转发队列又是单独的一层：Swift 侧凭据保存在钥匙串，Go 后端队列使用 AES-GCM 加密并按 Telegram/Bark provider 记账；发送失败保留待重试状态。provider 侧超时发生在对方已接收之后时，远端可能重复投递，不能声称服务商提供 exactly-once。

2.75的设备会话按线路和对方号码隔离，N待会话按Profile和对方号码隔离；未读及当前会话前台抑制共用`AttentionStore`。设备转发独立持久进度与N待独立游标仅在后端加密保存确认后前进。配置身份与入队在同一控制器锁下确认，目标变更暂停旧渠道任务；慢provider的网络阶段不持有队列状态锁，保留单worker与提交中保护。

2.76 的 N待短信展示使用 Rust 会话摘要索引和按时间向前加载的历史分页；展示游标与原有通知、转发 revision 游标独立。Telegram 固定任务分片并持久保存逐片确认，Bark 必须收到可验证的服务端成功响应；结果不确定时暂停，人工重试仍可能重复当前提交。N待报告分别记录 IMS 续期、计划 IKE 重认证和意外故障，REGISTER 租期仅采用本端绑定，缺 Contact 时保留兼容回退。

### 2.79 设备发送与归档导出

- `device_send_store.rs` 在独立 `device-send-ledger.json` 保存固定请求、正文、设备/订阅与进程内通道绑定。原子持久 claim 只授予一次；丢 ACK、重启与未知状态不自动重发。最多 10000 条/32 MiB，满后拒绝，不按短信历史清理授权墓碑。此账本含明文短信且限本机私有权限，与转发加密队列是不同数据。
- `device_sms_send.go` 使用捕获的 IMS session、modem 或 USB 通道，原通道变化不重新选线。AT 多段发送持有短信/SIM/voice 协调锁；modem 队列及写 PDU 前复核已观测代次，USB 复核已缓存身份，claim 前准备绑定 30 秒后失效，已获 claim 的排队执行继续复核实例与观测身份。它们是已观测变化保护，不能识别所有未报告的物理换卡。
- 新 IMS 短信的 subscription token 仅从原会话完整匹配的 ICCID+IMSI 派生；旧历史及 SM/ME 不回填。稳定消息 ID 纳入有效 subscription，新 ledger 发件另纳入 request ID；未知旧 ID 不改。Swift 用相同订阅选择原回复线，旧卡/未知历史不引用当前新卡号码。
- `DeviceSMSSendStore` 统一三个设备发送入口；`DeviceSMSRecoveryView` 只查询或显式继续尚未提交的原请求。“新发送”独立选线确认。通道接受不等于收件人送达。
- `sms_export.rs` 为两份归档各固定文件描述符快照，逐记录投影生成匿名 0600 导出，默认去除号码/正文/订阅，App 通过 256 KiB 分块写临时文件并原子替换用户选择的目标。每记录 ≤1 MiB、总输出 ≤128 MiB、最多四份、十分钟失效。每个来源截面独立，不是跨库事务；不改变消息、已读或转发游标。

### 2.80 协议与短信存储故障隔离

- 外层用户态 ESP 的每个 SA 独立维护 64 包抗重放窗口；先初检，在认证、padding 和 IP 长度检查通过后锁内再次确认并提交。合法乱序可接收，重复/过旧/零序号包拒绝。发送序号达到上限后拒绝继续使用该 SA；SWu 的 netstack/TUN 发送循环取消会话，由现有 reader/N待生命周期处理恢复，不新增 rekey 算法。
- GCM nonce 不再复用密钥子切片的剩余容量，避免相邻 key material 被原地覆盖；协议算法、协商参数与内层 IMS 保护不变。
- `sms_store_health.rs` 包装设备发件账本和 N待存储，各自锁存损坏、I/O、路径、上限或 worker 故障。同进程中已排队的提交也不能越过故障；不会将损坏当作新空库。启动恢复失败只禁用受影响 store，网关与 Go 子进程仍可运行；其他功能仍须根据各自状态判断。
- `GET /api/v2/runtime` 的 `ok` 仅表示网关可用。新增 `sms_store_health` schema 1，分别包含 `device_send` 和 `nwait` 的 `state/reason/restart_required`，不返回短信、号码或文件路径。受影响的正式短信 API 返回不可自动重试的 `503 sms_store_unavailable`；原记录保留，故障修复后须重启重新校验。N待停止、操作结果及历史运行报告仍允许访问，方便释放会话。
- 设备发件账本和设备收件归档是不同层：该隔离不将所有设备接收都停用，也不把高级 AT 控制台改成通用硬件写入沙箱。N待入站保存失败仍不能确认成功；不确定发送结果仍禁止自动重发。
- 诊断页按打开、后端状态变化或手动查询更新；旧后端缺少字段、未知 schema 或查询失败显示未确认，不沿用旧正常结果，不新增定时轮询。

### 3. 网卡优先级
- `GET /api/network/services`：解析 `networksetup -listnetworkserviceorder`
- `PUT /api/network/services-order`：`osascript do shell script` 提权执行（base64 编码避免引号地狱）
- 模块网卡识别：模块 USB 产品名（Baiwang）匹配 Hardware Port 名
- 前端拖拽排序：onDrag/onDrop + DropDelegate（**不要用 List+onMove，ScrollView 内嵌 List 渲染不可靠**）

### 4. 通话与音频
- DJI 模块使用 `AT+CLCC` 轮询通话状态（stat: 0=active,2=dialing,3=alerting,4/5=incoming,6=disconnected）
- 读卡器通话经 IMS 安全通道，媒体由 Swift CoreAudio 桥接；运营商未提供 PCMA 时允许协商 AMR，避免以错误的强制编码破坏呼叫
- Rust `proletarius-sim` 提供只读 SIM 身份能力：标准 SELECT FILE / READ BINARY APDU 构造及 EF-ICCID 半字节解码。Go 聚合 USIM 字段时会先关闭自身 PC/SC 句柄，再通过认证的 native identity endpoint 校验并采用 Rust ICCID；Rust 不可用时保留已验证的 Go 回退值。
- Rust `proletarius-reader` 的 AKA 锁按读卡器名称隔离，并以弱引用回收拔插设备锁，避免热插拔造成全局锁表增长。
- 自 0.80 起，Rust AKA 请求可指定 USIM/ISIM，在同一次 PC/SC 事务内从 EF-DIR 查找完整 AID 并认证；Go 不再为该路径预先打开卡或枚举 AID。Rust 服务未配置时仍使用 Go 兼容路径，ISIM 缺失保持显式回退语义。
- **无号码的 active 条目视为固件残留**（ATH/CHUP 清不掉），忽略
- 来电横幅（顶部 overlay）+ 拨号/挂断；状态卡住时轮询失败标记 "unknown"

### 5. 服务生命周期
- app 启动自动拉起后端、退出自动停止（无手动开关）
- `BackendProcess.start()` 的 guard 写法注意：`state == .failed("")` 永远不匹配，失败后无法重启——用 `if case .running = state, process?.isRunning == true { return }`

## 踩坑记录

1. **日期解析**：Go time.Time 输出 RFC3339Nano（可能带小数秒），Swift 需自定义日期解析（ISO8601 带/不带小数秒都试）
2. **SwiftUI 编译**：ViewBuilder 里不能直接 for 循环（用 ForEach）；`onChange` 双参数版本需 macOS 14+（目标 13 用单参数）；超长表达式导致编译器 type-check 超时（抽子视图）
3. **@main 命令行程序**：顶层表达式 + semaphore 在 `-parse-as-library` 模式会卡死（测试工具用 main.swift 顶层代码模式）
4. **List 在 ScrollView 内**：macOS 渲染不可靠（空白/塌陷），用 VStack+ForEach 替代
5. **AudioBufferList**：Swift 中 `mBuffers` 不是数组，用 `UnsafeMutableAudioBufferListPointer` 遍历；IOProc 参数是非 Optional 指针（可判 nil 但不能 if let）
6. **语音开关状态**：`AT+QCFG="usbcfg"?` 响应含回显行（`AT+QCFG="usbcfg"?`），解析必须用 LastIndex 或正则；末尾 `\r\nOK` 会污染 Split 结果（正则提取最稳）
7. **模块 UAC 音频设备名**是 "AC/AS Interface" 而非 "Baiwang"

## 构建

```sh
mise run build               # 构建 Apple Silicon / macOS 27 的 .app 并输出到 dist/
mise run launch              # 启动已构建的 app
```

需要：支持 macOS 27 arm64 的 Xcode、Go 1.26.3（由工作区脚本固定）、Rust 1.98、pkg-config、libusb（brew）。具体工具链和主机构建信息随优化基线记录。

## 测试

- `app/Tests/APIProbe`：无硬件环境 API 解码验证（服务连通、模型解析、错误路径）
- 真机测试：从 app bundle 启动 `switchd-rs -listen unix:<SWITCH_DATA_DIR>/test.sock`，经认证 Unix socket 验证读卡器、代理、IMS/SMS/通话链路

## Switch 隔离边界

直接打开应用时默认数据根为 `~/Library/Application Support/Switch`；开发脚本通过绝对路径 `SWITCH_DATA_DIR` 指向工作区 `build/runtime`。新多待原始短信、分片、回执与幂等发件记录由 Rust 持久化，与既有单卡短信模型分开。每个 Profile 以独立 SWu/netstack/IMS 代次保持网络会话；共享物理卡只在 FIFO 许可内完成身份检查、切卡和 AKA。该路径启用 SMSOnly，旧语音会话的默认协议行为保留。
