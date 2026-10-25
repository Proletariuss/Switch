# Iteration571：负向 INVITE 的独立事务生命周期

2026-10-19。**整体阶段：P5完善中，P6最终验收未完成；接口接管100%（294/294）单列。** 本文由预审更新为实施记录，关闭所述软件缺口；整体百分比仍待完整必需验收台账核定。本轮没有实卡操作、实网通话/短信、GUI/麦克风测试、应用安装或DMG。

## 问题与实现

原单项last_reinvite_reply可能被后续拒绝覆盖；491在先前200获得ACK后重放可能转为200。现改为信令Service持有的CompletedInvite表，与当前Call、媒体及退休dialog分离。最多64项，首次100或最终拒绝写入前预留，成功2xx移交独立Accepted生命周期时释放Proceeding槽。容量满不驱逐旧义务，不推进CSeq、准备媒体或发出无法冻结的final。Proceeding由原来电决策期限管理，不提前套用final的H。

覆盖带合法事务身份的初始拒绝与通话内400/405/415/422/481/486/488/491/500/503等出口，包括尚未创建Call的拒绝。冻结完整Response及首次writer结果：known重放完整原字节；unknown只吸收同事务重复，不用修改后的SDP重新决策。首次known的H从成功回执开始，unknown从写入尝试前开始；失败重放不抹掉首次成功事实、不改变旧codec或提前发送BYE。

在Accepted和普通dialog分派前匹配连接、Call-ID、From/To tag、CSeq、URI及原始顶层Via sent-by/branch。负向ACK必须使用原INVITE branch，不能误确认同CSeq另一分支的Accepted 200；真正2xx ACK保持原有匹配规则。TCP按H=64T1（32秒）及ACK后的I=0处理，立即清空并零化最终响应body。独立tombstone只拦截晚到负向ACK，保留至H；同dialog/CSeq仍有Accepted义务时延长分类保护，不继续重发负向final。依据[RFC3261 §17.2.1](https://www.rfc-editor.org/rfc/rfc3261.html#section-17.2.1)与[RFC6026 §7.1](https://www.rfc-editor.org/rfc/rfc6026.html#section-7.1)。

CANCEL有独立缓存、writer事实和期限，不确认INVITE、不延长其H。Completed时匹配CANCEL返回200且保持原final；I=0后若没有先前CANCEL结果则返回481。Proceeding CANCEL同时严格检查URI与To-tag，拒绝冒用原分支的其他请求。H已过而CANCEL仍存活时只调度其未来期限，避免返回过期H造成忙循环。shutdown显式清空所有事务。

普通180写入失败会尝试冻结原INVITE500并终结，不留下无期限Proceeding。初始失败释放已分配资源，即使final writer未知也等待一次实际stop；stop失败保留围栏。重协商prepare失败但discard成功时仅冻结488、保留原通话；discard也失败时才尝试一次BYE并等待实际释放，不让writer错误绕过清理。SDP版本溢出冻结500，不先改媒体。

## 验证与交叉审议

最终定向Rust回归 **301通过、0失败、0忽略**：call70、native_ims_calls190、production peer10、完整生产组合1、call_codec20、registered session7、call setup3。新增Completed owner测试24项，包含多分支与时钟矩阵。强制内核沙箱只允许本机回环，canary验证IOKit/user-client、Mach、设备文件和外部UDP被拒绝，子进程继承限制。

覆盖连续400/415/422/488/491/500及容量503、旧ACK后491不变、同CSeq不同Via、unknown首次/失败重放、无Call拒绝、busy后重复、终结/新Call隔离、H/I/独立CANCEL期限、64槽不驱逐及Accepted保护。核对完整响应字节和prepare/configure/start/stop/BYE次数；多个故障分支不能被测试数量误读为实机证据。

本地真实EVS/AMR-WB、IKE/ESP/IMS/RTP/PCM组合保留精确七条通话历史（六呼入、一呼出）。新增待ACK的200、不同CSeq及同CSeq不同Via的491、重复负向ACK/I=0之后200继续重传，随后合法ACK提交；多个拒绝交错重放时逐段核对真实双向媒体与端口/token/SSRC/序号/时间戳连续。旧Call结束和新Call中仍精确重放旧final，不能改变新媒体。

首轮回归发现两处实际/测量问题并保留失败证据：通用Uri解析器接受了INVITE Contact中的`*`，现由dialog Contact边界严格要求sip/sips且非wildcard，初始和重协商均冻结400；没有改变REGISTER通用URI解析。另原生产夹具OPTIONS由IMS actor提前回答，不能证明呼叫owner已处理前序INVITE；改为OPTIONS语法检查加bodyless INFO业务屏障，只有INFO回执及全部预期final到齐才推进。原完整字节、空剩余集合、七条历史及媒体断言未放宽。

进一步交叉审查发现Via原始字符串匹配会把TCP大小写、合法slash/colon空白或host大小写变化当成另一事务，使负向ACK漏过保护，或把Ringing的合法CANCEL返回481。现在TCP Proceeding/Accepted/Completed及refresh等入口共用结构化身份：协议和域名大小写、SP/HTAB、IPv4/IPv6数值归一化；branch仍严格区分大小写，缺失/重复/非法branch及非TCP拒绝匹配，冻结响应字节不重写。省略port与显式5060仍保留不同身份，不把网络默认值直接扩大为事务等价。坏Via ACK被吸收，不能落入Accepted确认。依据[RFC3261 §7.3.1](https://www.rfc-editor.org/rfc/rfc3261.html#section-7.3.1)、[§17.2.3](https://www.rfc-editor.org/rfc/rfc3261.html#section-17.2.3)和[§20.42](https://www.rfc-editor.org/rfc/rfc3261.html#section-20.42)。首次296项通过的旧源码/二进制、主动中断的旧候选构建和后续编译记录单独归档，不计入最终冻结结果。

对于已知负向branch，ACK的URI或额外tag严格不匹配时，仅按connection、Call-ID、CSeq及规范化Via键吸收，不确认另一Accepted事务，也不把B标成tombstone；正确B ACK才完成I=0，A的新branch ACK仍能确认200。新回归覆盖13种等价格式、9种坏Via、不同合法branch/host/port、普通Ringing重复INVITE/CANCEL及Accepted重复吸收，以及URI scheme/host大小写/真正不同URI和From/To tag变化。三处测试原本要求CallBinding实现Debug的断言在编译前改为普通相等断言，没有给敏感DTO增加Debug。最终重新冻结2860项输入、重编译后统一跑过301项。

Swift生产逻辑harness **29项模拟音频/API场景通过**，device-call正确性检查通过；没有实际TCC/麦克风验证。冻结协议 **297次回放通过**，6466个direct样本仅校验来源与完整性，不作回放计数。契约保持294 Rust/0 Go、54条HEAD、127 Swift调用与431个既有review范围；只明确加入新测试模块排除项。最终冻结2860项编译/测试输入，包含6项本轮新增/修改输入；冻结oracle不变。

## 边界与剩余目标

此包针对当前注册TCP信令，不宣称实现UDP G/I/T4、缺Via branch的完整RFC2543兼容或所有畸形报文事务。TCP顶层Via多值有回归，但unknown quoted extension中的逗号/分号、完整header folding和通用URI等价规则未作为完整SIP解析器验收。64槽过载保留既有义务并显式返回owner容量错误，尚不是完整外部过载控制协议。

native EVS内部初始化/分配与worker join故障仍待实质验证，fake prepare失败不能替代native部分初始化失败；已形成[下一包native故障预审](iteration-572-native-codec-failure-review.md)，仅分析未实施。运营商EVS接受、双向音质/来去电/DTMF、模块DSP仍待实网验收。正常LPA及9eSIM B0/B1认证下载、BLE恢复、双向短信与Telegram/Bark真实投递、多Profile/多读卡器N待、IMS/IKE/CHILD自然到期及24h/72h长稳保持完整目标。P6共享data-dir生命周期authority、旧3.42退出1、原位升级回退和受控整机性能未完成。

完整候选构建、包内实际daemon校验、缓存清理与最终门禁另记于`build/reports/iteration-571-*`；未完成报告不能作通过依据。

## 最终候选与缓存清理

完整SwiftUI Release候选构建及arm64、deep/strict签名校验通过。候选Bundle ID为`com.proletarius.switch.candidate571`，版本3.43/build343；签名为本机ad-hoc。包内后端只有switchd-rs和libusb，实际Rust二进制SHA-256为`0ac913755df19b884fd64ad1287b1c967df2dac0ef8e5b0158a593baa05481e2`。未启动GUI、替换已安装应用、改写正式dist或生成DMG。

对该包内实际二进制在强制无设备/外网沙箱中验证正常、满日志管道、关闭stderr三模式，均exit0且无socket残留；备注原始扩展和公开DTO保持。单次退出观察为0.0046/0.1407/0.0016秒，不作为整机性能或p95证据。

确认编译结束且逐路径无打开文件后，精确清理7项可再生成的编译缓存，共2,063,822,848分配字节（约2.06GB）；APFS可用空间观察增加1,836,490,752字节，分别登记。2860项冻结输入零漂移，源码、离线依赖、EVS资源、前后两版候选、正式dist与报告保持；测试home没有重复下载工具链，最终缓存dry-run为空。

[最终门禁](../../../../build/reports/iteration-571-gate-summary.json)、[源码审查](../../../../build/reports/iteration-571-source-review.json)、[候选校验](../../../../build/reports/iteration-571-candidate-app-verification.json)、[包内进程](../../../../build/reports/iteration-571-candidate-backend-process.json)、[缓存清理](../../../../build/reports/iteration-571-cache-cleanup.json)。本轮完成该软件工作包，项目目标继续保持active。
