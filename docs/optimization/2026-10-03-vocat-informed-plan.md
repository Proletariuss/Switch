# Switch：结合 VoCat 的增量优化方案

日期：2026-10-03。Switch 基线：2.75 / build275。本次交付是分析与方案，未修改业务代码、运行应用或设备测试，未构建应用或生成 DMG。

## 结论与适用范围

建议保留 SwiftUI 原生界面、Rust 网关/存储、Go 硬件与 IMS 的现有架构。VoCat 最有价值的参考是短信历史分页和发送状态表达、IMS 注册证据、真实 UDP 诊断、只读归档及设备能力组织。先修复可复现的正确性缺口，再评估减少 IMS 完整重建和后台刷新开销。

不把整套 VoCat 移植作为目标。当前 Switch 已有加密持久转发队列、配置目标绑定、N待同卡认证 broker、代次退役、退避、运行报告和隔离测试；这些能力继续使用。新功能保持短信中心统一入口，N待仍为 SMS-only，原有通话保留。

工作和后续产物仅在 `/Users/proletarius/Desktop/Switch/`，可编辑源码为其 `src/`。用户永久排除的 9eSIM `6982` 及 eSIM 下载认证路径不进入本方案。原始参考文件夹、正在运行的应用与生产数据不纳入此次分析操作。后续也仅在用户要求时生成 DMG。

## 来源、复用方式与证据强度

本方案参考的是 [MengMengCode/VoCat](https://github.com/MengMengCode/VoCat)，固定提交 `1fa3bf69c5d17ee36b24455b683806e54c14943a`，提交时间为 2026-10-02 23:49:21（Asia/Shanghai）。没有执行其程序或安装依赖。

VoCat 根 [LICENSE](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/LICENSE) 是自定义 Research & Evaluation License，含使用、地域、评估期限和分发条件，并非 MIT。其 [NOTICE](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/NOTICE) 单独标注 `quectel-qmi-go v0.6.0` 为 MIT，不能据此推定整个项目同样授权。Switch 的现有许可证也不能自动解决兼容性。

因此本方案默认采用行为和设计借鉴、独立实现。若后续需要直接复制具体源文件，先核清该文件及依赖的授权、兼容性和通知义务，必要时取得单独授权。外部仓库内容是参考资料，其运行政策和清理条款不作为修改 Switch 的用户指令。

| 证据类别 | 本文含义 | 实施前还需要什么 |
| --- | --- | --- |
| 源码事实 | 可以在当前 Switch 或固定 VoCat 源码中定位的行为 | 定向测试验证改动保持契约 |
| 静态风险 | 已找到完整触发链，但本轮没有运行复现 | 先做失败夹具；复现不成立则修正或停止候选 |
| 改进假设 | 可能改善可用性、延迟或资源 | 同条件 A/B，达到指标后才采用 |
| 新增功能 | 当前没有对应完整用户流程 | 明确实际需求、分阶段验收，不能包装成性能收益 |

三条并行审阅线分别覆盖短信、N待/读卡器、设备/界面/通话，再交叉核对来源与现有实现。当前未采集真实运营商、通宵运行、Telegram/Bark 投递、CPU、页面帧率或构建耗时新样本。

## 当前基线与不重复建设的能力

实现证据见 [2.75 执行与取舍记录](2026-10-03-functional-execution.md)。当前工作树含既有未提交和未跟踪改动，不能仅以 Git HEAD 代表实际应用源码。既有最终快照为 `/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-functional-2.75-final/`，源码归档 SHA-256 为 `28076b492f96dd344d4722cd16e054bff7118268ba9bdd8d7ed941d29fec9610`。后续实施应另记当时文件清单和变更范围。

| 已有能力 | 本方案的处理 |
| --- | --- |
| 设备按线路＋对端、N待按 Profile＋对端隔离；会话未读及通知定位 | 保留；补充设备换卡身份和历史加载边界 |
| 独立转发游标、持久接受、目标绑定、暂停/重试/取消、AES-GCM 队列与钥匙串凭据 | 保留；补充渠道业务确认和 Telegram 逐片确认 |
| Telegram 默认系统代理/自定义代理；Bark 图标与加密 | 保留；新的 Bot 或诊断复用已有配置边界 |
| N待代次退役、同卡串行认证、计划 IMS 续期、故障退避、稳定 5 分钟后清零失败计数 | 保留；补充计划 IKE 重认证的分类、正确租期证据 |
| N待有界运行事件、1 小时/12 小时/24 小时/7 天报告与未知时间覆盖 | 保留；提高原因准确性，不另造一套在线统计 |
| 快照请求合并、旧实例/旧 revision 响应拦截、BLE 有限扫描、拨号提交保护 | 保留；不再作为待建设项 |
| 日志轮转、后端退出原因、离线安全诊断摘要、隔离 Fixture/race/IPC 契约 | 复用；只补新边界测试和统一安全操作事件 |

既有转发 Fake Sender 测量中，本机入队/状态 p95 从 144.619 ms 降至 9.912 ms；短信模型分组本身约 0.505 ms。前者不代表服务商投递速度或 CPU 收益，后者不足以支持继续加分组缓存。本次未重新运行这些测量。

## 候选排序

成本为一名熟悉 Go/Rust/Swift 和当前实现的工程师估算，包含定向实现与回归；不含运营商等待、真实设备浸泡、授权或新硬件成本。成本区间不是交付承诺。P0 指优先验证的正确性问题，并不表示本轮已实机复现。

| 编号 | 优先级与候选 | 主要收益 | 估算成本 | 决策 |
| --- | --- | --- | --- | --- |
| C1 | P0：N待最新历史、会话摘要与分页 | 重启后最新消息和较早会话均可找到 | 完整 3–5 人日；最新页最小修复 1–2 人日 | 先复现，分阶段交付 |
| C2 | P0：计划 IKE 重认证独立归因 | 掉线/重连报告和退避不被计划事件污染 | 1–2 人日 | 先补事件夹具 |
| C3 | P0：匹配自身 Contact 的 IMS 租期 | 避免提前重建和零租期误报在线 | 1–3 人日 | 同会话续期的前置项 |
| C4 | P0：Bark 业务确认 | 无效响应不能被当作已投递 | 0.5–1 人日 | 按官方契约验证，兼顾自建服务 |
| C5 | P1：Telegram 逐片持久确认 | 后续片失败时减少已成功片的重复转发 | 1–2 人日 | 改动加密队列状态，不重造队列 |
| C6 | P1：设备换卡身份＋发送意图恢复 | 防止混会话、错默认回复和重启重复提交 | 身份 2–4；发送含设备账本 4–7 人日 | 分成两个独立交付 |
| C7 | P1：真实 UDP 往返与分层诊断 | 区分代理配置成功与实际数据可达 | 2–4 人日 | 显式诊断优先，不增加常规启动串行等待 |
| C8 | P1：白名单操作时间线 | 定位失败阶段，减少敏感原始日志依赖 | 3–5 人日 | 先最小事件结构，再页面展示 |
| C9 | P2：同会话 IMS REGISTER 续期 | 减少 IMS 租期到达时的完整会话重建 | 5–10 人日 | 条件试验，默认关闭，分阶段评审 |
| C10 | P2：只读短信归档/导出 | 可核查、迁移和备份历史 | 2–4 人日 | 有使用需求时新增 |
| C11 | P2：设备能力、USSD、信号与网络选择 | 暴露现有后端能力，改善操作反馈 | 各子项单独估算 | 分阶段，不捆绑成为一次大改 |
| C12 | 条件：非关键 UI 刷新预算 | 可能减少后台资源和重复请求 | 测量 0.5–1 人日 | 达标才试验，完整 SSE 暂缓 |

### C1：N待历史加载先保证完整，再讨论搜索

**源码事实与静态风险。** [Rust messages 查询](../../rust/crates/proletariusd/src/multistandby_store.rs) 按 revision 升序取页，默认页大小 100；[Swift refreshSMSCenter](../../app/Sources/MultiStandbyStore.swift) 每个 Profile 仅用空 cursor 取第一页。中心消息索引在内存中，而 changes cursor 持久化。超过 100 条历史、重启且 changes cursor 已追到尾时，可能出现新历史未装入中心的触发链。这里讨论的是显示/加载遗漏，不能称为归档数据已丢失。

借鉴 VoCat [loadMore 的时间＋ID 游标及滚动锚点](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/web/src/pages/SmsPage.tsx#L411)，分别建立最新消息快照、覆盖全部会话的可分页摘要索引，以及按会话/订阅作用域加载的 history cursor。仅把最早 100 条改成最新 100 条仍会使更早的安静会话从列表消失，因此不把最新页修复当作完整历史交付。完整索引与分页预计 3–5 人日，包含最新页最小修复；后者单独预计 1–2 人日。保留既有 revision/change-feed、删除墓碑、通知与转发提交语义，不把历史游标当变更游标使用。

验收：150/250 条以上归档重启，最新消息可见；一个活跃会话超过 100 条、另一个会话只有更早短信时，重启后两者均可找到；按会话继续加载能到达其全部旧消息；同时间戳、删除、新接收并发不重复或遗漏；通知目标在未加载页时可定位，不因此重播历史通知或补转发。先用失败 Fixture 验证上述静态触发链。

全文历史搜索是单独的新功能。当前 Switch 搜索只覆盖联系人、线路及最后预览；VoCat 的 [filterThreads](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/web/src/components/sms/smsText.ts#L113) 也不是全文搜索。若确有需求，再实现服务端正文查询和结果定位，约 3–5 人日，使用 1k/10k/100k 合成档案测量，不先引入数据库替换或缓存。

### C2、C3：先保证续期事实和在线证据正确

**C2。** Switch [watchSWU](../../backend/cmd/proletarius-macos/reader_vowifi_session.go) 当前将 SWu 错误统一归为 `tunnel_lost`；[SWu session](../../backend/third_party/swu-go/pkg/swu/session.go) 的计划完整重认证会返回 `ErrReauth`。因此计划事件可能进入意外掉线统计和故障退避。先用 `ErrReauth`、真实隧道错误、正常停止、手动重试四类夹具复现，再增加 `planned_ike_reauth`，独立记录计划 IMS 续期、计划 IKE 重认证和意外失败。

验收：计划事件不增加连续失败次数；原有安全等待、退役顺序、稳定窗口和真实故障退避不变。VoCat [orchestrator 分类](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/vowifi/orchestrator.go) 可作参考，其到达 SMSReady 即清零策略不替换 Switch 已有的稳定窗口。分类修复只提高统计正确性，不自动代表重连次数减少。

**C3。** Switch [finalizeRegisterSuccess](../../backend/internal/vowifi/imscore/register.go) 从多个 Contact 取最短正租期、忽略零值；其他设备的短绑定可能导致本会话提前续期，本端零租期也可能被默认值掩盖。参考 VoCat [applyRegistrationEvidence](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/vowifi/ims/provider.go#L1456) 的自身 binding 选择和零租期撤销语义，独立实现结构化 SIP Contact 解析与归属判断，不照搬字符串包含匹配。

验收：本端 3590 秒＋他端 60 秒、本端 `expires=0`、全局 Expires、缺失/歧义 Contact、多个 instance/URI 格式夹具。协议明确的本端零租期撤销注册；证据缺失按运营商契约说明保守策略。短信 feature tag 用于诊断，不作为所有运营商的强制门槛。该项必须先于 C9。

### C4、C5：转发确认只补缺口

**C4。** Switch [Bark Sender](../../backend/internal/smsforward/providers.go) 读取正文后主要以 HTTP 2xx 判成功，需验证异常或业务拒绝正文是否会错误推进渠道完成状态。VoCat [通知请求帮助函数](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/server/settings_api.go#L788) 也存在类似边界，不能把它当现成修复。按 [Bark 官方服务实现](https://github.com/Finb/bark-server/blob/master/route_push.go) 核定兼容响应契约；官方服务本身会用 HTTP 错误码表达失败，不能宣称其正常失败总是 HTTP 200。

验收：正常成功、明确业务拒绝、非法 JSON/HTML、响应丢失、超时和认证失败分别处理；明确拒绝不记已投递，响应不确定不伪装为确定失败或成功。错误持久记录仅保留安全枚举，不记录短信、Key、URL Token。加密载荷和图标能力不改动。

**C5。** Telegram 已有长文本分片，缺口是每片成功没有独立持久确认；后片失败可能重发前片。新增绑定消息、目标配置版本和固定片内容的确认进度，继续放入现有 AES-GCM 状态，不把正文写入偏好设置。VoCat [telegramTextRequest](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/server/telegram_bot.go#L2473) 的截断策略不适合本项目。

验收：“第二片失败→全进程重启→重试”仅发送未确认片；配置改变、暂停和恢复不改变原目标；Unicode 和长度边界正确。服务已接受而响应丢失仍可能重复，不能承诺远端 exactly-once。

### C6：发送证据和订阅身份

**设备换卡身份。** [SMSModels](../../app/Sources/SMSModels.swift) 的设备会话键使用线路＋对端，[receivedSMS](../../backend/cmd/proletarius-macos/main.go) 未带稳定订阅身份。同一设备换卡、同一对端的消息可能仍归入旧会话。借鉴 VoCat [deriveThread](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/web/src/components/sms/smsText.ts#L82) 的设备＋订阅＋对端模型，增加从当前已验证卡身份派生的 opaque subscription ID。

接收、发件、归档、未读、通知定位和默认回复同步升级；原始 ICCID/IMSI 不进入偏好键或分享诊断。历史无身份记录保留“身份未知”，不能用当前卡信息补写。验收两张卡、同一设备、同一对端互相隔离，旧通知不能默认从新卡回复。N待已有 Profile 隔离，继续使用。

**发送意图与结果。** N待已有后端持久 request ID 和未知结果语义，但 Swift 的 pending send ID 主要在内存；设备路径主要返回 `sent:true`，UI 展示“发送成功”。参考 VoCat [发送结果表达](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/web/src/pages/SmsPage.tsx#L56)，统一 `queued / submitting / accepted_unconfirmed / delivered / partial / failed / unknown` 的 DTO 和用户文案。SIP 200 或模块提交成功不等于收件终端送达。

N待复用现有后端幂等能力，补受保护的前端发件 intent 与按 `client_request_id` 查询结果。设备发送入口尚未接受该 ID、没有对应持久请求账本，需要同时增加持久账本与原子发送 claim，绑定请求 ID、载荷摘要和订阅身份，在硬件提交前落盘；相同 ID 的不同载荷必须拒绝。执行中重启标记为 unknown，不能自动重放。只有 Swift intent 和查询接口不足以保证设备路径不重复逻辑提交。

重启后先查询，复用同一 ID 和固定线路；未知状态由用户选择，不自动再发。用户明确“新发送”才创建新 ID。发送恢复含设备账本预计 4–7 人日，先交付 N待恢复，再单独验证设备原子 claim。验收提交成功但响应丢失、重启、线路切换和再次点击仍只形成一次逻辑请求；持久写入失败不得调用硬件，执行中崩溃不得自动重放，旧未知结果不能阻止用户明确发起新的请求。不把新功能宣传成远端绝对零重复。

完整分段送达报告关联另计 5–8 人日，必须拿到段引用、订阅、发送尝试和有效窗口。VoCat [ApplySMSDeliveryReport](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/store/sms.go#L397) 可参考聚合行为，其“最新匹配 TP-MR”不可直接照搬，8 位引用复用可能误配。证据不足则保留“未确认送达”。分段预估使用 Switch 实际编码器，不另用 UI 字符数猜测。

### C7：UDP 与服务链路分层诊断

Switch [proxy_probe](../../rust/crates/proletariusd/src/proxy_probe.rs) 的 `udp_ready` 当前主要来自 UDP Associate，不能证明数据实际往返。VoCat [ProbeSOCKS5](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/proxy/probe.go#L38) 进行真实 UDP DNS 请求，并校验响应身份。

建议区分 TCP 连接、代理鉴权、Associate、真实 UDP 往返、ePDG IKE、ESP、IMS/SMS 就绪。DNS 成功只证明该探测目的的 UDP 往返，不证明运营商链路。Telegram 的 HTTP 代理能力也不能替代 N待所需 UDP 能力。

探测放在显式诊断或失败排查，用总 deadline 和可配置目的，不默认复制第三方公共 DNS/域名。维持 SOCKS 域名模式避免本地 ePDG DNS 的边界，禁止静默直连降级。验收 Associate 成功但 UDP 黑洞、错误来源、响应 ID 不符、截断、通配 relay、IPv4/IPv6、取消和控制连接关闭均不误报；正常启动无额外串行等待。不能产生可行动结论则停止扩展。

### C8：白名单操作事件和诊断时间线

参考 VoCat [有界日志](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/loghub/hub.go) 与 [操作审计](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/server/audit.go)，增加操作类型、随机关联 ID、代次、阶段、错误枚举、持续时间和结果。诊断页可筛选，复用现有安全摘要导出和轮转机制。

当前 [modem manager](../../backend/internal/modem/manager.go) 的部分原始 logger 输出号码、卡片标识和 USSD 文本，[IMS register](../../backend/internal/vowifi/imscore/register.go) 也有身份/Contact 输出。这里只读源码，没有读取真实日志。先减少未来敏感原始输出，再建立结构化事件；不要直接把现有原始错误、SIP、AT 或 APDU 加入可分享时间线。

验收固定容量、状态不变不刷事件；合成手机号、验证码、Token、APDU 和嵌套错误不能进入导出；失败能定位到等待锁、连接、认证、提交、退役等阶段。非关键日志订阅可丢旧显示，短信归档/操作完成事实仍须独立可靠保存，不能用覆盖式日志 hub 承载。审计写入异常不得使硬件锁长期不释放。

### C9：同会话 IMS 续期作为独立试验

当前 [watchRegistrationLease](../../backend/cmd/proletarius-macos/reader_vowifi_session.go) 在 IMS 租期续期点触发 generation 退役和重建。VoCat [refreshOnce](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/vowifi/ims/provider.go#L1567) 在原 Session 中 REGISTER，并保留 `qop=auth` 状态、推进 nonce count。

这项潜在收益最大，但不是修改定时器即可完成。Switch 需要明确 digest/CSeq 生命周期、SIP 事务分发、并行入站短信、安全协商和 readiness。新 AKA 挑战仍经 [multiAKA / broker.withProfile](../../backend/cmd/proletarius-macos/multistandby_card.go)，不得跨 Profile 或代次复用凭据。旧会话、SPIs、nonce 和迟到响应必须有归属边界。

只替换 IMS 租期续期路径。IKE SA rekey、CHILD SA rekey、计划完整 IKE 重认证及卡片认证租约保持各自流程。失败、明确零租期或安全协商变化撤销 readiness，并回退已有完整退役/重建，不持续展示虚假在线。

实施分两阶段：先用最多 2 人日验证接口和协议 Fixture 可行性；成立后重新核定剩余预算，总体预计 5–10 人日。开关默认关闭。后续指定同卡同 Profile、同运营商进行至少 3 个自然续期窗口及 24 小时对照，记录观测覆盖率、计划重建数和每次 not-ready 持续时间。

拟定采用门槛：计划 IMS 续期导致的完整重建次数和未就绪累计时间均下降至少 50%，无错卡认证、短信遗漏/重复或死锁；非计划断线恢复不回退。没有真实基线或运营商行为不支持则暂缓/关闭开关。24 小时是试验观测长度，不是通用永久在线证明；IMS 注册租期也不是套餐有效期。

### C10、C11：按实际需求新增功能

**只读归档。** 借鉴 VoCat [流式快照](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/store/sms_export.go#L16) 和 [导出端点](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/server/sms_export.go#L48)，支持线路/Profile/会话/时间范围、版本化 JSON、可选离线 HTML。显式选择号码遮盖策略，原子完成文件，HTML 转义、无外部脚本。验收并发接收快照一致、大档案不全量堆积正文内存、磁盘失败不呈现成功，导出不改变未读、通知、转发或历史游标。诊断摘要不替代短信归档。

**设备能力，2–4 人日。** 借鉴 VoCat 的 [unsupported capability](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/device/types.go)，扩展现有非下载能力 DTO，统一 `available / unsupported / busy / unknown`、原因和代次。VoCat 本身也没有完整细粒度能力矩阵，该目标属于 Switch 增量设计。验收至少 12 种状态组合，切卡旧结果不能开启新卡操作；N待短信不依赖语音就绪。现有 eSIM 能力和下载认证实现不改。

**蜂窝 USSD，4–6 人日。** Switch 已有 [USSD Provider](../../backend/internal/backend/ussd.go)，但 macOS API/UI 尚未形成完整流程。先接原生“运营商服务”面板，绑定设备/订阅/代次，明确开始、继续、取消和结束。补底层取消边界、多轮输入、取消失败、迟到 URC 和切卡隔离。IMS [USSD 当前为占位](../../backend/internal/vowifi/voiceclient/ussd.go)，VoCat [USSI runtime](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/vowifi/ims/ussi_runtime.go) 仅作另一阶段参考，先花 3–5 人日验证可行性，再估接入成本；不承诺全部运营商，不自动猜测套餐续费，也不静默切到其他 Profile/传输。

**信号详情与网络选择。** Switch DTO 已有高级信号字段，但采集不完整。先用 1–2 人日显示实际可用 RSRP/RSRQ/SINR、单位、时间及未知值；新增采集若需要协议适配另计，不伪造已有值。人工扫描/选网约 3–5 人日，复用已有 [operator selection Provider](../../backend/internal/backend/operator_selection.go)，沿用通话、SIM 和 VoWiFi 占用保护，回读确认并提供恢复自动选择。打开页面不自动扫网，不能把 running/终态包装成百分比实时进度。

**AT 反馈，1–3 人日。** 借鉴 VoCat [AT grammar tests](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/server/at_guard_test.go)，补单行、长度、CR/LF/NUL 和组合命令处理，区分模块 `ERROR/+CME ERROR` 与运输故障。保留高级控制台和已有取消/占用保护，不使用永久 force 开关，不宣称黑名单能识别所有厂商命令。改变设备状态的命令显示具体影响，避免不透明的通用提示。

**只读 Telegram Bot，3–5 人日。** 仅在有远程查看需求时新增 `/status /unread /queue`，正文查询另设开关，复用现有代理/凭据。参考 VoCat [authorized](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/server/telegram_bot.go#L2329)，使用数字用户/chat 白名单、持久 update 进度和有界 worker。默认关闭；未授权不返回数据；只读命令不能切卡、拨号或发短信。VoCat 的内存 offset、每 update 新建 goroutine 和整套远程 AT/发短信控制不一并引入。

### C12：刷新、SSE 与资源成本

VoCat [usePolling](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/web/src/lib/usePolling.ts) 在页面隐藏时暂停，但 [概览 SSE](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/server/device_features_api.go) 仍由两秒 ticker 生成快照。SSE 不是自动节能的证据。

Switch 已有单请求合并和 revision/实例拦截。先测可见、隐藏、最小化各 3×10 分钟的 App/Rust/Go CPU、RSS、唤醒、每类 HTTP/IPC 和实际 AT/APDU 次数。只降低非关键 UI 刷新，窗口恢复立即更新；短信、N待与来电后台检测继续独立运行。

达到既有 CPU 门槛才开展最多 1 人日的优化试验；预计剩余适配超预算时重新评审。HTTP 请求减少不能代替资源收益。完整 SSE 暂缓，只有重复轮询成为瓶颈才考虑有序事件 feed；必须保留 generation、revision、断线恢复、丢事件校验和周期性对账，不能替换短信持久 change feed。

## 后置候选与明确取舍

| 候选 | 当前决定 | 重新开启条件 |
| --- | --- | --- |
| Carrier 有效配置预览、差异、来源和回滚 | 可后置，约 2–4 人日；沿用已有严格 YAML、Rust 解析及 revision | 有明确配置管理障碍；Go/Rust 夹具一致且非法配置不可应用 |
| 本地 IPCC 导入 | 暂缓；包含导入器预计 5–8 人日 | 具体 carrier 需求；转换与安装分开、有界解析、来源校验与回滚。参考 VoCat carrier_ipcc 设计，不整体复制 |
| 初始 ePDG 候选切换 | 暂缓，原型 2–4 人日 | 已有首候选失败样本；仅认证前同 carrier 候选，在总预算内切换，成功后固定 SA 目的。成功率提升 ≥10 个百分点或启动 p95 改善 ≥20%才继续 |
| 批量重连 jitter | 暂缓；VoCat 当前也没有该能力 | 同卡批量恢复实际排队拥塞；峰值压力下降 ≥30%、恢复 p95 恶化 ≤10%才继续 |
| 全面 Rust 重写、SwiftUI 换 Web、整体改 SQLite | 停止泛化提案 | 独立热点证明现有架构无法小步修复且迁移成本可回收 |
| VoCat Linux XFRM、iptables、sysfs、QMI 数据面整体搬入 macOS | 不采用 | 与当前平台边界不符；按 macOS userspace/netstack 路径处理 |
| 用 VoCat runtime 替换同卡 N待 | 不采用 | 其 [runtime manager](https://github.com/MengMengCode/VoCat/blob/1fa3bf69c5d17ee36b24455b683806e54c14943a/internal/vowifi/runtime/manager.go) 以 deviceID 管理单 orchestrator，未找到 Switch 同卡多 Profile 认证 broker 的等价设计 |
| VoCat 通知游标和失败跳过策略 | 不采用 | 不替换 Switch 更完整的持久接受/加密队列；部分渠道失败不应静默跳过短信 |
| 额外分组缓存、队列容器微优化、构建缓存工程 | 维持此前停止/暂缓决定 | 新测量达到既有门槛，不因别的项目采用就引入 |
| 套餐到期自动续费、通用永久在线承诺、9eSIM 6982 | 不进入本方案 | 租期与套餐不同；6982 按用户指令永久排除 |

## 指标、成本阈值与测量方法

以下保留 [既有综合方案](2026-10-02-comprehensive-optimization-plan.md) 的收益规则，并补充 C9 的候选门槛。均为目标而非本次实测结果。

| 领域 | 固定条件与样本 | 采用门槛与业务保护 |
| --- | --- | --- |
| 正确性 | 同一源码起点的失败 Fixture；覆盖重启、并发、身份与取消 | 先复现后最小修复，不要求达到 CPU 门槛；每次最小修复先限 2 人日，超出重新评估 |
| 空闲资源 | 同机 Release、同设备与网络，前台/隐藏/最小化各 3×10 分钟；三进程合计 | CPU 相对下降 ≥20% 且绝对下降 ≥0.5 个百分点，超过样本噪声；RSS 仅有真实增长/压力时立项 |
| 短信/队列延迟 | 固定数据与并发条件，至少 6 次预热＋40 次计时，记录 p50/p95；真实 UI 绘制另测 | p95 改善 ≥25% 且 ≥50 ms；不能把模型/入队速度当端到端投递 |
| 消息发现与来电 | 同线路、同设备的事件时间序列 | 新短信发现 p95 不得恶化超过 5% 或 250 ms，取更严格者；来电事件 p95 ≤1.5 秒；显式断链撤销 ready ≤3 秒 |
| CPMS/APDU | 固定读卡器、卡/固件和邮箱数量，记录调用、占用及排队 p95 | 调用减少 ≥80% 且有 CPU/排队收益，消息发现不回退；无硬件样本则不改调度 |
| IMS 同会话续期 | 正确租期基线；至少 3 次自然续期＋24 小时同条件观察，记录 coverage | 计划完整重建和未就绪累计时间各下降 ≥50%，无协议/短信/错卡回退；先 Fixture，再实机 |
| 界面 | 代表性 5/20/50 Profile、长短信，首屏/切页/滚动分开采样 | 延用 p95 响应 <100 ms、95% 帧 ≤16.7 ms 目标；性能重构需重复测得 >20% 且 ≥8 ms 收益 |
| 构建 | 冷构建、无改动、仅 Swift 改动各 3 次，固定工具链，分别记录阶段 | 中位数改善 ≥20% 且 ≥5 秒，90 天节省 ≥工程＋维护成本的 2 倍；本次不构建 |

一次只验证一个假设。普通性能候选试验先限 1 人日；新协议或功能采用独立分阶段预算，不自动以“优化”为由扩张。连续两轮未过门槛、收益处于噪声、成本超预算或出现消息遗漏、串 Profile、重复硬件提交、死锁、误报就绪，即停止/回退并记录取舍。

记录字段统一为：候选编号、实际源码快照、设备/网络/数据、事实或假设、原始样本、基线和改后指标、工程与维护成本、实施差异、定向回归、实际未测边界、采用/停止/暂缓及原因。新的功能收益用已解决的具体用户任务验收，不杜撰 CPU 加速百分比。

## 建议实施批次

1. **先验证 C1–C4。** 建失败 Fixture，再分别修历史快照/会话索引、计划重认证归因、本端租期、Bark 确认。含完整历史索引与分页总估算约 6–11 人日；先交付 1–2 人日的最新页最小修复，再核定历史索引阶段。逐项交付、逐项重新判断，不先改数据库或定时策略。无法复现的候选保留证据并降级。
2. **再推进可靠性和可诊断性。** C5 逐片确认，C6 先订阅身份再发件恢复，C7 真实 UDP，C8 最小安全事件。按各项预算独立推进，不用一次大 PR 跨三种语言改变所有公共模型。
3. **有数据后试 C9/C12。** C9 依赖 C2/C3；C12 依赖资源和时效基线。先小范围、可关闭试验，未过线就维持现行实现。
4. **新功能按使用频率选择。** 优先离线归档；再按需求选设备能力、蜂窝 USSD、信号详情、人工选网或只读 Bot。IMS USSI 和 IPCC importer 单独立项，不合并承诺低成本全支持。

每批复用现有隔离 Fixture、Go race、Swift Store 和 Rust/Go IPC 契约，只新增本批有意义的边界测试。通过定向回归再做必要的完整 Release 验证；无新增问题不重复扩大测试。真实卡片、短信、代理和服务投递另记录指定条件，不以本机 Fixture 冒充运营商验收。DMG 仍等待用户明确要求。

## 本次实际交付

仅新增本方案。研究记录位于 `/Users/proletarius/Desktop/Switch/build/research/vocat-20261003/`，包含固定提交元数据、文件树、文本参考及清单；不混入生产源码或依赖。

方案交叉复核后补明确了两项边界：最新消息页不能代替全部会话摘要索引；设备发件恢复必须有后端持久账本和原子 claim，不能只新增前端保存。对应成本和验收已调整。文档本地文件链接与固定提交参考路径已核对。

本次没有修改业务代码/版本，未运行第三方代码、测试、构建或应用，未访问真实短信、凭据或生产日志，未触碰读卡器和号卡。源码静态风险、预计人日和拟定指标均需在后续实施时验证。
