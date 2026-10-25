# Iteration 568：通话内 Accepted 事务隔离

2026-10-19。**整体阶段：P5完善中，P6最终验收未收口。接口接管100%（294/294，包含54条HEAD）为独立指标。** 按用户要求，每次进度回复标注整体阶段、本轮新增与剩余验收；历史人周账本尚未核平，完整必需验收分母未冻结，不编造整体百分比。见[整体进度表](progress-current.md)。本轮没有实网呼叫、短信、写卡或麦克风测试，没有替换已安装应用或生成DMG。

## 已实现

将通话内re-INVITE的Accepted身份与待提交SDP、媒体分开保留。每个dialog最多16条Accepted记录，另维持最多16个retired dialog；未到期或仍有清理义务的记录不因新事务、挂断、媒体失败或新通话而被驱逐。容量满在prepare前返回503/Retry-After，保留原拒绝响应。

首次200交给writer之前保存完整响应和事务身份，回执后锚定Timer L=32秒和TU重传起点；已知成功200按0.5秒指数退避、上限4秒定时重传，包括TCP。匹配INVITE静默吸收，不触发即时200或重复媒体提交。ACK按所属连接、dialog和CSeq匹配，一次停止相应TU；已ACK记录继续保留至Timer L。旧ACK不能消费新pending、回滚Contact/租期/SDP或重新prepare/configure媒体。严格匹配的晚CANCEL返回200，错误连接481，未知To-tag的INVITE不再误当新来电。

已成功写入200后的媒体提交失败，保留原Accepted身份及TU，不重新发送BYE或复活媒体。首次200写入未知则保留身份、禁止重放可用200，清理媒体并保持uncertain；严格ACK或32秒期限只执行一次原dialog清理。候选Contact仅用于所属dialog最终清理，不修改当前新通话。BYE标记先于写入；即使BYE写失败，仍等待当前媒体清理并保留写入错误和stop失败围栏。对仍占用资源的失败owner，合法清理事件可触发一次stop重试，重复事件不再发送BYE或重复重试。

当前及retired事务的TU由独立调度器处理，终态页面不会停止必要重传/清理；旧dialog清理只使用旧连接、路由和Contact。shutdown即使stop失败也清零所有Accepted表与SIP定时器，同时保留未确认释放的媒体围栏。

上述行为参照[RFC6026 §7.1](https://www.rfc-editor.org/rfc/rfc6026.html#section-7.1)及[RFC3261 §13.3.1.4](https://www.rfc-editor.org/rfc/rfc3261.html#section-13.3.1.4)；这里没有宣称完整SIP事务矩阵已经完成。

## 验证与边界

定向Rust回归 **225通过、0失败、0忽略**：call70、native_ims_calls117、production peer7、完整生产调用链1、call_codec20、registered session7、call setup3。新增17项owner故障回归，覆盖连续事务、严格ACK、writer回执、未知写入、prepare/commit/discard/stop失败、offerless ACK、BYE写失败、两级容量、retired隔离及shutdown。所有Rust回归在强制内核沙箱执行，IOKit及user-client、设备服务/设备文件、外部网络被拒绝，仅本机回环开放；canary验证限制及子进程继承。

完整本地IKE/ESP/IMS/RTP/PCM链路中，两个方向分别执行六次顺序重协商；每次ACK及后续UPDATE/488后重放所有旧Accepted INVITE/ACK，以同一TCP的双OPTIONS屏障验证无响应，同时继续验证真实EVS/AMR-WB、PCM、RTP端口/SSRC/序列/时间戳及started_at连续性。挂断后、新通话期间和最终BYE后也执行旧事务重放。静默oracle优先于ACK缓存，专属反作弊回归证明非法重复200不能被缓存吞掉。

生产peer重放仅覆盖已ACK成功事务，未ACK/未知写入与故障清理由owner故障测试覆盖；peer时间戳晚于server writer receipt，不能把发送前32秒检查当作真实Timer L边界测量。真实到期和容量释放由owner的受控时间回归验证。本轮首轮Rust回归116/117通过：一个旧测试仍期待未知200超时后不发BYE；补充严格ACK的一次旧dialog BYE、目标/双方tag与重复消息静默断言后，全组117/117通过。失败记录保留，原discard/stop/错误围栏断言未降低。

冻结协议 **297次回放通过**；6,466个direct样本仅核对完整性和来源，不计作本轮执行。契约保持294 Rust/0 Go、127 Swift调用及431个既有审阅范围，只刷新三处行号和新增测试模块清单；既有review指纹、冻结oracle及Swift生产代码未改。最终编译/测试输入2,855项，构建、签名、包内daemon测试与缓存清理详见`build/reports/iteration-568-*`最终报告。

## 剩余范围

完整多non-2xx/Completed事务及拒绝响应的长期身份保留仍待完成，不能把当前单条non-2xx缓存外推为全覆盖；初始无offer/可靠临时应答及剩余writer故障矩阵继续推进。实网来去电、双向音质/EVS接受、真实DTMF/记录、模块DSP仍待产品验收。

下一软件包只读预审已确认：初始空body INVITE当前被488拒绝，answer要求已有remote，初始ACK也不解析SDP，因此不能只删除空offer拒绝；须实现保留本地offer及严格ACK answer的独立状态。另需先封住不具备可靠UAS状态时对Require:100rel和未关联PRACK的处理，避免未经RAck关联的SDP进入普通media_update。这些尚未修改或测试，不计入本轮完成项。

P6共享数据目录生命周期authority、旧3.42退出1、原位升级回退及受控整机性能未关闭；正常LPA及9eSIM认证/下载兼容、BLE恢复、真实双向短信与转发、多Profile/多读卡器N待、自然IMS/IKE/CHILD期限及24h/72h长稳继续保留在整体目标中。接口接管和本地回归不能代替这些验收。

[最终门禁汇总](../../../../build/reports/iteration-568-gate-summary.json)、[源码审阅](../../../../build/reports/iteration-568-source-review.json)、[候选包校验](../../../../build/reports/iteration-568-candidate-app-verification.json)、[缓存清理](../../../../build/reports/iteration-568-cache-cleanup.json)。

## 候选构建

完整SwiftUI Release候选应用构建、deep/strict签名、arm64及包内动态依赖检查通过；独立Bundle ID为`com.proletarius.switch.candidate568`，版本3.43/build343。包内后端仅switchd-rs与libusb，最终Rust二进制SHA-256为`6aeaf037cc285b390eb5e7b614d7822bf4e604a3794306555fd044aeafca3ee5`。该实际二进制在强制无硬件/外网沙箱下，正常、满管道及关闭stderr三种进程启动/退出均exit0，无自有socket残留，原始备注扩展和公开DTO保持；退出耗时分别为0.0046/0.1433/0.0015秒，属于单次观察，不是整机性能p95验收。未启动GUI或访问真实音频设备，正式dist和已安装应用不变。

验证完成且无进程/打开文件占用后，精确删除7个编译缓存路径，合计1,996,451,840分配字节（约2.00GB）；APFS可用空间观察增加1,786,306,560字节，分别记录以免重复计数。本轮未重新下载重复Rust工具链。2,855项输入、当前候选、正式产物、EVS与报告保持，离线依赖保留。
