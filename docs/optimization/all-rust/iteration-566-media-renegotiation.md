# Iteration 566：通话内媒体重新协商与失败屏障

2026-10-18。本轮实现 [Iteration 565](iteration-565-p5-renegotiation-residual.md) 中的 SDP/媒体重新协商工作包，并通过离线生产调用链验证。**这不是完整 P5、P6 或全项目 100% 验收。** 未修改已安装应用，未生成 DMG；`dist/Switch.app` 仍为 Iteration 563 的受测产物。

## 生产实现

入站 re-INVITE、UPDATE/PRACK 的新 offer 与本机发起交换的 answer 分开验证；answer 对照本次实际保留的 offer，检查 PT、AMR 封装和 mode-set、方向约束及 DTMF 事件子集。三个实际 answer 入口使用对应远端 offer 构造响应，offerless re-INVITE 则保留本地 offer/ACK answer 交换。旧 Go 冻结预期及旧 parser 投影保持不变。

媒体采用 prepare/commit/discard：先准备解码器及接收上下文，成功写出 200 后提交发送格式，不等待 ACK 才切换。re-INVITE 的 200 按 0.5/1/2/4 秒重传，等待 ACK 最多 32 秒；重复消息不重复配置媒体。UAC 的刷新 answer 在等待 ACK 写入前应用媒体约束。写入结果不确定、提交失败及 codec 终止错误进入清理/占用屏障；已建立通话必要时发送 BYE。

同一 RTP/PCM 所有者支持 EVS↔AMR-WB、AMR mode-set/period/neighbor 与 OA/BE（不同 PT）切换，以及 EVS 同 PT 的 hf-only 限制变化。接收端最多保留 8 个旧配置、通常最多 60 秒，仍被有界重排队列引用的旧上下文在排队包处理完前保留。每个排队包绑定原接收配置，同端点共享排序；底层 native 编码器保存权威发送模式和帧相位，避免较新上下文先过期后恢复过时模式。配置变更不替换端口、token、SSRC 或重置 RTP 时钟。

当前 EVS 实现只声明实际支持的 Primary 13.2/WB、单声道、20ms、`dtx=0`、`cmr=-1`。新严格路径验证双向 br/bw 子集、显式参数及合法 hf-only 协商；对端明确要求未实现的 DTX、CMR/EVS IO 行为时，选择合法 AMR-WB 或拒绝，无共同编码时不会暗中启用 NB/PCMA。AMR 发送和接收均按保留 offer 检查显式 transport 参数，生成侧也保留显式默认值。规范依据为 [TS 26.445 Annex A](https://www.etsi.org/deliver/etsi_TS/126400_126499/126445/18.00.00_60/ts_126445v180000p.pdf) 与 [RFC 4867 §8.3.1](https://www.rfc-editor.org/rfc/rfc4867.html#section-8.3.1)。

Contact 与 SessionTimer 在成功 200 写入回执时生效；初始 answer、无正文 UPDATE、re-INVITE 和带 SDP UPDATE 都从该回执确认的时刻计算期限，准备媒体或等待 writer 的时间不会消耗新租期。ACK 不覆盖后来 UPDATE 接纳的 Contact/timer。该起点按 [RFC 4028 §9](https://www.rfc-editor.org/rfc/rfc4028.html#section-9) 核对。

媒体 stop 失败仍发布 failed/uncertain，取消所有 SIP deadline/pending，保留资源占用屏障及原始业务错误；后续 tick 不重复 BYE，shutdown 可重试清理而不伪造清理成功。无正文 UPDATE 的 200 写入未确认也进入失败屏障；后续 UPDATE/PRACK 返回 481，迟到初始 ACK 或 re-INVITE ACK 不会复活失败通话。

## 验证

最终冻结 **2,851 项构建/测试输入，零漂移**。三名 High 子代理分别承担媒体、SDP/线级夹具及 SIP owner 交叉审阅，修正首轮失败、协议能力声明和最终失败注入问题；根代理统一构建、执行与收口。审阅指纹及具体取舍见 [source review](../../../../build/reports/iteration-566-source-review.json)。

| 最终定向组 | 通过数 |
| --- | ---: |
| proletarius-call（SDP/codec） | 70 |
| native_ims_calls（owner/真实媒体） | 81 |
| 生产 peer 的 ACK/UPDATE oracle | 4 |
| 完整 IKE/ESP/IMS/RTP/PCM 呼叫组合 | 1 |
| call_codec | 20 |
| native_registered_session | 7 |
| native_ims_call_setup | 3 |
| 合计 | **186** |

上述 Rust 测试均在强制内核沙箱执行，只允许本机网络；IOKit（含新的 user-client 操作）、Mach 设备服务和外部 UDP 被 canary 验证拒绝，限制继承到子测试进程。没有无沙箱回退。组合测试注入模拟卡，实际使用 native EVS/AMR-WB 编解码器：两个对话方向各执行 6 次 re-INVITE 和 2 次 UPDATE，每阶段发送 4 帧，逐帧检查双向 PCM 接收和 RTP 连续性，并在阶段内确认双端产生非静音 PCM；同时检查零解码错误、稳定端口/token/开始时间；UPDATE 的 200 不依赖 ACK。非法 PT 重绑的 re-INVITE/UPDATE 返回 488 后，再发真实语音帧验证旧媒体仍可用。独立媒体测试还覆盖旧 RX 排队、模式/相位过期竞争及同 PT hf-only 来回切换。缺少实际 codec 会测试失败，不作为 skip 通过。

另有 **297 次冻结协议回放**、78 项验证工具测试通过；6,466 项 direct fixture 只检查了完整性/来源，未把它们宣称为本轮执行次数。NB/PCMA 的历史 fixture 用于旧协议投影，不代表生产开启该编码。全 Rust 格式检查、结构 Go 退出检查及契约检查通过：294 Rust / 0 Go 路由、127 Swift 调用、54 HEAD、431 review scopes；本轮只刷新 census 中行号和新增测试模块，不改已有 scope 指纹。最终构建及回放放置 Go 调用陷阱，未触发。

另编译当前真实 daemon（SHA-256 `5f72ec418769dada69a270c64e097e70a71d9eb0a209d182ba2e65662ef720db`），在强制无硬件/外网沙箱中验证 available/full/closed 日志管道：均正常退出 0，分别约 0.0046/0.1449/0.0015 秒，无自有子进程/socket 残留，备注原始扩展字段保持。该单次合成退出观测不属于受控性能 p95。

详细证据见 [最终验证](../../../../build/reports/iteration-566-final-verification.json)、[冻结输入](../../../../build/reports/iteration-566-final-source-snapshot.json)、[定向组日志索引](../../../../build/reports/iteration-566-final-groups.json)、[daemon 过程](../../../../build/reports/iteration-566-daemon-process.json)。前期失败和后续重新冻结的结果分别保留，只有最终冻结结果用于以上通过结论。

## 后续完整目标

下一软件工作包是初始呼入 200 自动重传/ACK 超时、ACK 前接收就绪，以及挂断/CANCEL/BYE 竞争；不能用本轮的通话内重新协商替代初始呼叫生命周期验收。完整 P5 仍需实网来去电、双向语音与音质、实际运营商 EVS 接受情况、硬件/模块 DSP、DTMF/记录等产品流程验收。EVS IO、完整 EVS DTX/CMR 扩展没有启用或计入完成。

P6 的生产共享 data-dir 生命周期 authority、未包装旧版历史、3.42 退出 1、原位升级/回退和整机性能仍保留。普通 LPA、BLE 重连、收发/转发、多 Profile/多读卡器 N待、自然 IMS/IKE/CHILD 租期与 24h/72h 长稳仍须按原方案分别验收。9eSIM 6982 继续排除。

本轮验证后确认没有构建/测试进程或缓存文件占用，再按白名单删除 Rust/Python 缓存，占用合计 1,288,142,848 字节（约 1.20 GiB）。离线依赖、EVS 资源、源码快照、测试证据、旧版回退材料与现有发行包保留；清理后冻结输入零漂移，`dist/Switch.app` 后端哈希不变。调试 daemon/测试二进制随可再生缓存删除，哈希及执行日志保留在最终验证 JSON，可从源码重建。已安装应用及 `dist/Switch.app` 未替换，不把测试二进制当成已发布版本。
