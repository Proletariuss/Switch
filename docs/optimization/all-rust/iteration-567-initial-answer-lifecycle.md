# Iteration 567：初始呼入接听与媒体生命周期

2026-10-19。**整体阶段：P5完善中，P6验收未收口。** 接口接管100%（294/294，包含54条HEAD）是独立指标；当前没有可信的整体加权百分比。后续每次回复按[整体进度表](progress-current.md)报告阶段、新增证据及剩余验收。本轮没有实网呼叫/短信、写卡、应用安装或DMG。

## 行为变化

初始来电在发送200之前实际prepare接收器；200写入成功后configure一次并进入只接收模式，ACK仅放行TX，不重新配置或替换RTP/PCM所有者。EVS与AMR-WB均有实际codec/RTP接收回归；ACK前语音和DTMF上行被阻止。Swift音频桥在incoming connecting阶段附着播放，收到ACK后的active状态复用同一连接启麦，覆盖ACK先于异步附着完成和挂断后迟完成的竞争。

初始200按T1=0.5秒指数退避至T2=4秒，等待ACK最多32秒；重传保留完整原始响应，不重建SDP/Contact/Session-Expires，起点以writer成功回执为准。Accepted身份与当前媒体分开保留：结束通话或新通话接管后仍吸收旧INVITE，旧ACK不会复活媒体。最多保留16个retired记录与当前记录，容量满时拒绝新准入，不驱逐仍有清理义务的旧记录。

同一初始Accepted事务内重复INVITE被吸收，200只由TU定时重传；即使使用TCP也保留端到端重传。已发送200的本地挂断、媒体提交失败等在ACK或事务超时后发送一次BYE；远端先BYE后迟到/过期ACK不再发送第二个BYE。匹配的晚CANCEL返回200而不撤销已接受的呼叫，错误连接返回481。上述刻意修正旧行为，依据[RFC3261 §13.3.1.4](https://www.rfc-editor.org/rfc/rfc3261.html#section-13.3.1.4)、[§15.1.1](https://www.rfc-editor.org/rfc/rfc3261.html#section-15.1.1)和[RFC6026 §7.1](https://www.rfc-editor.org/rfc/rfc6026.html#section-7.1)。

200写入结果未知时保持failed/uncertain，清理媒体、静默吸收旧INVITE，不重放200或自动启动媒体；严格匹配的ACK可证明对端已接收，此时只尝试一次BYE清理。stop失败保持资源占用/uncertain围栏。shutdown关闭TU与回复缓存，即使stop失败也不继续安排SIP定时器，不伪造资源释放。

## 软件验证

最终定向Rust回归 **206通过、0失败、0忽略**：call70、native_ims_calls100、production peer5、完整生产调用链1、call_codec20、registered session7、call setup3。初始呼入专属17项覆盖真实writer回执、严格ACK/连接匹配、T1/T2、ACK前挂断、失败/退休事务/容量、未知写入及shutdown；真实媒体新增EVS/AMR-WB接收与停止失败围栏。

Rust测试执行于强制内核沙箱，仅允许本机网络；canary确认IOKit（含user-client）、Mach设备服务、设备文件与外部UDP被拒绝，限制继承给子进程。完整本地IKE/ESP/IMS/RTP/PCM调用链在初始ACK前收到真实AMR-WB语音、确认无上行，至少收到两次相同200后才ACK；首个和重复ACK后双向帧连续，端口/token/SSRC/序列及已开始时间保持。BYE后迟ACK再经同一TCP上的OPTIONS屏障确认不恢复媒体。

Swift轻量harness直接编译生产CallAudioStore/CallModels，**24/24场景通过**，覆盖显式挂断后非终态轮询/迟ACK不得重启、跳过idle的新callID/token/port替换、旧附着/许可晚到、reader状态失败回落module/unknown时立即停止旧音频，以及模块权限返回后副作用前取消；音频/API均为mock，不访问真实麦克风或网络；现有device-call正确性检查通过。另有297次冻结协议回放及78项验证工具测试通过；6,466个direct fixture仅核对完整性/来源，不计为本轮执行次数。原冻结oracle未改。契约依然294 Rust/0 Go、127 Swift调用、431审阅范围；只刷新行号与新增测试模块清单，既有review指纹未改。

首轮编译暴露测试API误用（Response无Display、Snapshot无Debug及测试值所有权），修正后重新编译。首轮native测试99通过/1失败，失败为新测试错误使用ringing公开状态；生产DTO为incoming，修正并保留内部tracker Ringing及100/180响应断言后，最终100/100通过。失败记录保留，未降低生产准入或删除覆盖。最终交叉审查另外修复Swift的四个实际竞争：挂断等待期间后发轮询重开音频、跳过idle复用旧reader连接、模块迟到麦克风授权仍发backendStart，以及reader故障回落module/unknown后旧麦克风残留。另覆盖reader直接回落已active模块时停止旧reader并仅启动一次模块路由。harness由最初10项增至最终24项，完整候选重新构建，未将早期10项通过外推为最终代码已验证。

具体最终二进制、源码指纹、应用构建/退出及缓存清理以`build/reports/iteration-567-*`最终报告为准。正式`dist`及已安装应用保持原版本，本轮隔离candidate只用于验证。

## 未关闭范围与下一步

本轮不等于所有INVITE事务已完成RFC6026接管：通话内re-INVITE尚有“重复请求直接回复缓存200”、多个Accepted事务及退役保留等边界，需要独立修正及故障回归。初始无offer/可靠临时应答、完整非2xx事务与writer故障矩阵也须逐项核对，不能将17项定向回归外推为完整P5。

实网来去电、双向音质及运营商EVS实际接受、模块DSP、真实DTMF/记录仍待产品验收。P6生产共享data-dir生命周期authority、旧3.42退出1、未包装历史、原位升级回退、受控整机性能保持未完成；正常LPA、BLE重连、双向短信/转发、多Profile/多读卡器N待、自然IMS/IKE/CHILD期限与24h/72h长稳仍按原方案验收。本次更新后的整体目标不再单列9eSIM6982排除项；9eSIM认证/下载兼容性保留为待核查与待验收，不计作完成。

## 最终构建与清理证据

最终冻结2,854项编译/测试输入，验证后及清理后零漂移，Go调用陷阱未触发。完整SwiftUI Release候选应用（独立Bundle ID）构建、deep/strict签名、arm64架构及包内动态依赖检查通过；后端目录仅有switchd-rs与libusb，未运行完整GUI或真实音频设备。最终包内后端SHA-256为`e6f7f05306eb8276d61ac99958bd6c30e433e0861ffbeacf8fd1c80e64badd03`，已对该实际二进制重做强制无硬件/外网沙箱退出：available/full/closed均exit0，分别0.0037/0.1459/0.0015秒，无自有socket残留，备注原始扩展保持。该单次观察不是性能p95验收。

构建完成且无进程/打开文件占用后，精确删除2,079,662,080字节编译缓存，并删除已与全局原件核对的1,413,468,160字节重复Rust工具链（含额外文档）；本轮合计3,493,130,240分配字节，APFS实际空间变化单列。保留源码、离线依赖、EVS、当前candidate、正式dist、历史基线和全部报告。验证wrapper明确固定现有RUSTUP_HOME，避免隔离home再次下载工具链。源码/正式产物指纹保持不变。

[最终门禁汇总](../../../../build/reports/iteration-567-gate-summary.json)、[源码审阅](../../../../build/reports/iteration-567-source-review.json)、[候选包校验](../../../../build/reports/iteration-567-candidate-app-verification.json)、[缓存清理](../../../../build/reports/iteration-567-cache-cleanup.json)。
