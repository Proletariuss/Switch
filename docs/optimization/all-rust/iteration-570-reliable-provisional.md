# Iteration570：可靠来电应答与早期dialog维护

2026-10-19。**整体阶段：P5语音完善中，P6最终验收未完成；接口接管100%（294/294，含54条HEAD）单列。** 整体百分比仍须完整必需验收台账核定。本轮没有实卡操作、实网通话/短信、GUI/麦克风测试、安装应用或生成DMG。

## 实现

初始来电要求100rel时，发送带真实SDP、RSeq和Require的可靠180，替代569的420未支持处理。仅Supported:100rel仍沿用普通180，现有UAC PRACK路径保持。新增独立可靠响应与PRACK事务记录：首次writer前预留，成功回执后启动T1指数重传，32秒未确认结束原INVITE；可靠1xx重传不套用2xx的T2上限。保留完整响应字节，连接、dialog、RAck、PRACK自身CSeq及Via分别验证。依据[RFC3262](https://www.rfc-editor.org/rfc/rfc3262.html)和[RFC6337](https://www.rfc-editor.org/rfc/rfc6337.html)落实以下三条初始来电流程。

- INVITE带offer：180中answer，PRACK空body，PRACK200空body。
- INVITE带offer：180中answer，PRACK携带新offer，PRACK200中answer。
- INVITE无offer：180中本地真实EVS/WB offer，PRACK中answer，PRACK200空body。

三条流程完成早期offer/answer后，最终INVITE200及ACK均无SDP。待ACK状态显式区分NoSdp和AnswerToLocalOffer，不能再从原INVITE是否空body推导。用户在PRACK前接听只保存意图、返回incoming，不抢先发送最终200；PRACK200成功且媒体提交完成后再执行该意图。最终ACK只开放上行，不重建codec worker或重复协商。

带offer的180成功回执后配置接收、start(true)，空PRACK不重复配置；PRACK新offer在其200成功回执后替换媒体。无offer的180前准备真实native能力池，PRACK有效answer及200成功后才选定端点/codec并开启接收。全过程沿用EVS优先、AMR-WB兼容及全PT历史约束，最终ACK前不开放语音或DTMF上行。snapshot可显示真实接收就绪，state保持incoming；Swift仍在接听后connecting阶段才附着接收，active后才申请麦克风。

PRACK已知最终响应精确重放，未知writer结果仅吸收同事务重复；含400/481/500错误final，不能通过同一事务修正RAck或更换body把拒绝变为200。缓存最多16项，未确认可靠响应时最多占15个错误槽，为合法PRACK保留一槽；满时不发无法冻结的final、不推进CSeq或SDP，也不驱逐旧结果。RAck身份可保留到可靠180首次成功后的64秒，并在终结时保证至少额外32秒；PRACKfinal保留32秒。这是有界本地保留策略，不能混同IMS租期。current/retired admission受这些未到期义务保护，shutdown显式清空并零化body。

接听现在establish既有early dialog，保留早期UPDATE成功接纳的Contact、远端CSeq及计时器完整事实，不从最初INVITE重建或重置期限；PRACK不刷新Contact/租期。早期CANCEL或BYE分别完成自身事务与原INVITE487，即使其中一次writer失败也尝试另一响应，最终只清理一次媒体。早期媒体失败、UPDATE写入失败和期限到期发送相应原INVITE final并清理，不提前发UAS BYE；final200后的ACK/timeout清理保持独立。stop失败保留资源围栏，显式shutdown可重试。

## 验证

最终定向Rust回归 **276通过、0失败、0忽略**：call70、native_ims_calls166、production peer9、完整生产组合1、call_codec20、registered session7、call setup3。其中新增可靠owner测试24项，包含多分支writer、媒体失败和known/unknown事务矩阵。所有Rust测试执行在强制内核沙箱下，仅允许本机回环；canary验证IOKit/user-client、Mach、设备文件、外部网络被拒绝以及子进程继承。

完整本地IKE/ESP/IMS/RTP/PCM生产组合在原四通话基础上新增三次可靠来电，精确核对七条不同ID的历史（六呼入、一呼出），设备、号码、answered、时间及不重叠区间均匹配。可靠180至少两次完整字节一致；早answer无最终200，错误RAck无媒体变化，真实EVS/WB早期接收和ACK前零上行验证通过。最终空200/ACK、重复/恶意PRACK、旧WB PRACK在新EVS通话中重放、同端口/token/SSRC/序号/时间戳连续、BYE后旧事务及OPTIONS屏障均核对。它是合成SIM/运营商的生产实现组合，不能替代真实运营商和音质验收。

Swift生产逻辑harness **29项模拟音频/API场景通过**，现有device-call正确性检查通过；没有实际TCC、系统音频路由或麦克风测试。冻结协议 **297次回放通过**，6466个direct样本仅核对来源/完整性。API清单保持294 Rust/0 Go、127 Swift调用与431个既有审阅范围；只加入新测试模块排除清单，既有review指纹未变。冻结2858项编译/测试输入，最终构建与缓存清理后另核对漂移；不修改冻结oracle。

## 交叉审查与首轮失败

独立审查发现错误PRACK final未冻结，可能被同事务修改后接纳；已补所有已关联dialog的错误结果及15+1槽容量回归。另发现原INVITE final缓存32秒后可靠180身份仍保留，旧INVITE可能触发180；现在仅未终结INVITE可回放可靠180，虚拟时间负例核对到期后静默、晚PRACK仍响应且不重启媒体。

首编有两处测试误要求含token的Snapshot实现Debug；改测试错误提取，未给生产Snapshot增加Debug。首轮运行一个旧PRACK负例缺RAck，准确的新语法校验返回400；为原“合法未关联请求返回481”的测试补有效RAck，保留481与所有无状态变化断言。错误日志和当时源码/测试二进制指纹单列保存，最终通过只计冻结后重跑。

## 剩余目标

下一包为[负向INVITE/Completed预审](iteration-571-completed-invite-review.md)：491未冻结可在旧ACK后变200、多non-2xx结果覆盖、不同Via branch负向ACK可能误确认Accepted，以及准入拒绝/Completed CANCEL的独立事务表。这些尚未实施，不计完成；本轮只关闭所述初始UAS可靠1xx和早期刷新软件工作包，P5仍未全部关闭。

native初始化/join故障注入、实网来去电、运营商EVS接受、双向音质/DTMF与模块DSP仍待完成。正常LPA及9eSIM/B0/B1认证下载、BLE恢复、真实双向短信和Telegram/Bark投递、多Profile/多读卡器N待、自然IMS/IKE/CHILD期限及24h/72h长稳保持完整范围。P6生产共享data-dir生命周期authority、旧3.42退出1、原位升级回退与受控整机性能仍未验收。

候选构建、实际包内daemon验证、缓存清理和最终门禁见`build/reports/iteration-570-*`，以已完成报告的状态及源码/二进制指纹为准。

## 最终候选与缓存清理

完整SwiftUI Release候选构建、arm64与deep/strict签名检查通过；Bundle ID为`com.proletarius.switch.candidate570`，版本3.43/build343。包内后端仅switchd-rs及libusb，实际Rust二进制SHA-256为`f07bb36cdd017c9c533766d1b05eea213bf58bf18445ffc3103594b40100545e`。对该包内实际二进制做强制无硬件/外网沙箱启动退出，正常、满管道、关闭stderr三模式均exit0、无socket残留，备注原始扩展与公开DTO保持；单次退出观察0.0015/0.1445/0.0016秒，不作为整机p95。未启动GUI、替换已安装应用或正式dist。

确认所有构建结束、逐路径无打开文件后，精确删除7项可再生成的编译缓存，共2,061,058,048分配字节（约2.06GB），APFS可用空间观察增加1,850,396,672字节，分别登记。2858项编译/测试输入零漂移，源码、离线依赖、EVS资源、前后两版候选、正式dist和报告保持；没有重复下载工具链。最终缓存dry-run为空。

[最终门禁](../../../../build/reports/iteration-570-gate-summary.json)、[交叉审查](../../../../build/reports/iteration-570-source-review.json)、[候选校验](../../../../build/reports/iteration-570-candidate-app-verification.json)、[缓存清理](../../../../build/reports/iteration-570-cache-cleanup.json)。
