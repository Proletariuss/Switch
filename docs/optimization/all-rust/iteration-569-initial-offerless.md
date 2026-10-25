# Iteration 569：初始无 SDP 来电与媒体准备

2026-10-19。**整体阶段：P5语音完善中，P6最终验收未完成；接口接管100%（294/294，含54条HEAD）单列。** 整体百分比仍须完整必需验收台账核定。上一轮568已完成并留有源码、测试及候选产物证据，本轮属于继续实施。没有实网呼叫、短信、写卡或麦克风测试，没有安装应用或生成DMG。

## 实现

初始空body INVITE现在可进入普通100/180来电流程。用户接听后，200中发送冻结的真实本地offer，默认EVS优先并广告AMR-WB，`evs_only`沿用既有能力门控。所有广告PT的意义在200前写入dialog历史，未被最终选择的PT也不能在后续重协商中改绑为其他codec或framing。待确认状态保留连接、完整dialog、CSeq、原始200、32秒期限和offer/answer角色。

新增`Media.prepare_local_offer`，不构造虚假的远端地址。200发出前初始化实际广告格式的native接收worker，复用同一RTP/PCM端口和token；预确认接收窗口最多32包、每包payload不超过2048字节，仅保留广告音频PT及合法framing，不提前解码、处理CMR或上行语音/DTMF。有效ACK按实际200 offer验证SDP、方向、PT/history及codec参数后，才确定远端endpoint和格式。缓冲包再按最终来源、PT和policy筛选并送入原重排队列，复用选定worker，关闭并join未选能力。最终configure/start各一次；重复或退休ACK不能重配媒体，也不能影响新通话。

Swift音频桥允许合法incoming connecting状态凭有效端口、非空token及就绪信令提前附着接收连接；此时不申请麦克风、不播放本地回铃音。ACK后active且mediaReady才在同一连接启麦。挂断revision、call identity、原始状态、端口/token与endpoint围栏仍约束迟到附着、许可返回及路由恢复。

首次initial200的pending清理意图现在在writer调用前保存。成功回执后锚定200定时重传；未知写入禁止周期重传可用200，但严格ACK或32秒到期仍须清理一次所属dialog。未知200后的清理BYE写失败也等待媒体stop；后续确认静默吸收，不能再次写BYE或返回`ims_call_dialog_terminated`。stop失败保留实际资源/uncertain围栏，合法清理事件或显式shutdown可重试释放。

初始本地offer等待ACK answer期间，新带SDP UPDATE或re-INVITE返回491，不修改offer/history/媒体；bodyless UPDATE仍可刷新Contact和会话计时器，后续ACK不回滚这些已接受状态。缺失或非法ACK answer、configure/start失败会尝试一次BYE并清理；即使BYE写入失败，也不跳过媒体释放。

当前尚无UAS可靠临时响应状态，因此Require:100rel在响铃/分配媒体前返回420及Unsupported:100rel；仅Supported:100rel仍可普通180。没有匹配可靠响应的PRACK返回481，先于通用CSeq/SDP处理，禁止未关联的PRACK改变媒体。这是[RFC3262 §3](https://www.rfc-editor.org/rfc/rfc3262.html#section-3)要求的严格未支持/未关联处理，**不计作完整UAS可靠1xx支持**；现有UAC收到可靠1xx后生成PRACK的路径保留。

## 软件验证

最终定向Rust回归 **252通过、0失败、0忽略**：call70、native_ims_calls142、production peer9、完整生产调用链1、call_codec20、registered session7、call setup3。新增20项owner回归、5项真实媒体回归及2项peer oracle；所有执行在强制内核沙箱下，仅允许本机回环，拒绝IOKit及user-client、设备服务/设备文件与外部网络，canary验证限制及子进程继承。

owner覆盖真实200字节及writer回执、EVS/WB按实际offer选答、32秒ACK期限、错误连接/tag/CSeq、非法SDP/重复ACK、全PT历史、prepare/configure/start/stop失败、CANCEL/hangup/BYE、未知200和BYE写入、新旧call隔离，以及pending offer的491与bodyless UPDATE刷新。这里的32秒是SIP事务期限，不是号卡VoWiFi注册租期。

真实媒体回归验证native EVS+WB或EVS-only pool在200前就绪、32包窗口/非法PT筛选、ACK后worker身份复用、WB mode2实际首帧FT2，以及实际PCM/RTP/DTMF的SSRC、序号和时间戳连续。源码交叉审阅未直接注入native初始化或线程join失败，不能把FakeMedia的失败覆盖外推为这些native故障已经实测。

完整本地IKE/ESP/IMS/RTP/PCM生产组合保留原有来去电、CMR、重协商和Accepted重放，并新增EVS优先与WB选择两次独立offerless来电。每次至少观察两次相同200后才确认；ACK前有四个真实语音包预收，无PCM播放或voice/DTMF上行，ACK后全部合规包经同端口/token输出，再进行双向真实语音与DTMF。正常及恶意重复ACK、旧EVS ACK在新WB通话中重放均不改变当前codec、started_at或连续性；BYE后OPTIONS屏障及socket/lease退休检查保持。最终历史精确为4条不同ID、3呼入/1呼出，按时间顺序对应四次完整通话，号码、answered、设备及不重叠区间均核对。

Swift轻量harness直接编译生产CallAudioStore/CallModels，**29/29模拟音频/API场景通过**，包含原24项及新增offerless双endpoint、ACK在附着期间、挂断/迟ACK、失效状态/端口/token/信令与路由恢复。现有device-call正确性检查通过；没有真实TCC、音频路由或麦克风验收。

冻结协议 **297次回放通过**；6,466个direct样本只核对完整性/来源。契约保持294 Rust/0 Go、127 Swift调用与431个既有审阅范围；仅刷新行号及新增测试模块清单，既有review指纹与冻结oracle未改。冻结2,856项编译/测试输入，构建、签名、包内实际daemon退出及缓存清理以`build/reports/iteration-569-*`最终报告为准。

界面晚附着的追加复核发现，原`receive_ready`在host尚未认证时仍解码并丢弃PCM，可能丢掉ACK后首批音频。本轮补充每call最多32帧、每帧精确640字节的PCM FIFO；无host时仍推进真实decoder/PLC，满时仅保留最新32帧，合法token（含仅prefix附着包）到达后按序交付。FIFO及临时输出使用Zeroizing，stop/drop清零；非法token不能flush或启动上行。新增两项EVS/WB真实回归逐帧比对独立native decoder，包含40帧容量及PLC、确认先于host附着、后续第5帧连续性及新旧call/token隔离。生产组合的WB leg也改为ACK后确认RX4/host0，再验证错误token无效、正确token取回全部4帧；首EVS leg保留ACK前附着，覆盖两种时序。32帧窗口之外的旧音频会被丢弃，不声称无限或可靠补播。

## 首轮问题与修复

第一轮新owner测试的ACK helper误用了去电dialog的remote-tag，而原呼入From tag为remote，导致六项失败、三项等待超时；修正测试构造，保持生产严格匹配及错误身份负例，并对预期wire加2秒上限。首轮完整生产组合已走完新增两次通话，但末尾仍断言原有2条历史；改为精确4条及逐项断言后重新通过。失败输出保留，未通过放宽生产身份或使用“至少N条”隐藏问题。

交叉审阅另外发现Swift原条件不能提前附着offerless接收，以及未知200清理BYE失败后的重复ACK会误报终止dialog；这些生产问题及晚host附着首包丢失均修复并增加相应回归。最终统计只计冻结后的成功重跑，不把首轮失败或旧二进制计入最终通过。

## 后续验收

完整UAS可靠临时应答仍需独立RSeq/RAck、可靠响应重传/期限、PRACK事务缓存和offer/answer角色；完整多non-2xx/Completed身份与writer故障矩阵也未关闭。本轮关闭初始offerless的相应软件工作包，不代表P5已完成。

实网来去电、运营商EVS接受及双向音质、真实DTMF/历史、模块DSP/语音恢复继续验收。P6共享data-dir生命周期authority、旧3.42退出1、原位升级回退及受控整机性能未关闭；正常LPA及9eSIM认证/下载兼容、BLE恢复、真实双向短信与转发、多Profile/多读卡器N待、自然IMS/IKE/CHILD期限与24h/72h长稳仍保留在目标中。

[最终门禁汇总](../../../../build/reports/iteration-569-gate-summary.json)、[源码审阅](../../../../build/reports/iteration-569-source-review.json)、[候选包校验](../../../../build/reports/iteration-569-candidate-app-verification.json)、[缓存清理](../../../../build/reports/iteration-569-cache-cleanup.json)。

下一包预审（仅设计，未实施）：UAS可靠1xx需显式记录最终ACK的O/A角色，不能继续从初始INVITE是否空body推导。首个可靠1xx含offer/answer后须按真实PRACK完成O/A；最终200/ACK不能重复发起协商。应复用现有early dialog的establish并保留已接纳CSeq/Contact，禁止重建dialog回滚早期UPDATE。独立RSeq/RAck与PRACK事务缓存、T1指数退避且不封顶T2、64T1期限、终态/退休晚PRACK及未知writer围栏均为待实现要求；参照[RFC3262](https://www.rfc-editor.org/rfc/rfc3262.html)及[RFC6337](https://www.rfc-editor.org/rfc/rfc6337.html)。

## 最终候选与清理证据

完整SwiftUI Release候选构建、deep/strict签名、arm64及动态依赖检查通过；Bundle ID为`com.proletarius.switch.candidate569`，版本3.43/build343。包内后端仅switchd-rs与libusb，最终Rust二进制SHA-256为`92feab310b86ac62203a34ebb81e5d7952c733377e210f202ef0930193084ee9`；前一份未含host缓冲的候选/测试证据单列pre-host-buffer，不代替最终产物。对最终实际二进制重做无硬件/外网内核沙箱进程测试，正常/满管道/关闭stderr均exit0，无自有socket残留，原始备注扩展与公开DTO保持。退出单次观察分别0.0046/0.1270/0.0015秒，不作为整机p95验收；未启动GUI或访问实际音频设备。

构建完成并确认无进程/打开文件占用后，精确删除7个编译缓存路径，共2,060,423,168分配字节（约2.06GB）；APFS可用空间观察增加1,833,648,128字节，分别记录。没有重建重复Rust工具链。2,856项编译/测试输入零漂移，源码、离线依赖、EVS、候选、正式dist及全部报告保留。

额外已识别的下一包边界：现answer重建dialog可能回滚用户接听前bodyless UPDATE的Contact/CSeq；需与UAS可靠early O/A一并修正并分别验证普通及100rel路径，当前不计为已关闭。
