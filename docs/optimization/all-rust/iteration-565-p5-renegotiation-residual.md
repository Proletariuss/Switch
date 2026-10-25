# Iteration 565：P5 媒体重协商剩余缺口审计

后续状态：下文为实施前审计快照。通话内媒体重新协商已在 [Iteration 566](iteration-566-media-renegotiation.md) 实施并完成定向离线验证；初始呼叫生命周期、实网/硬件及完整 P5/P6 的其余验收仍未完成。以下“未实现”不作为当前实现状态。

日期：2026-10-18。本文是当前工作区的只读源码审计及后续实施清单，**没有实现媒体重协商，没有运行构建、测试或实网呼叫，也没有生成 DMG**。本轮先处理 Iteration 564 的数据与日志 P1 缺口；P5 的下面工作仍然保留，不能因本次记录而计为完成。源码行号是审计时快照，后续改动应按函数名复核。

## 1. 原目标及当前边界

[原全 Rust 计划](../2026-10-04-all-rust-backend-plan.md) §5 的 P5 是“完整语音”，包括 IMS 对话/媒体、EVS 优先及 AMR-WB 回退、能力门控、音频桥、来去电及接听/取消竞争、DTMF、停止/恢复、记录，以及另行验收的模块 DSP 策略。EVS IO 若启用，还必须有真实编解码与模式切换证据。不能把这份后续软件工作包缩小为完整 P5 的替代品。

[Iteration 563](iteration-563-amr-wb-cmr.md) 已实现同一 AMR-WB 编码器、既定协商集合中的 CMR 运行时码率调整、period/neighbor 限制及 RTP 连续性。它不等于 SDP 重协商：当前协商集合、封装和 codec family 仍固定。563 自身已明确，仅关闭 CMR 软件缺口，不表示 P5、P6 或整体 100%。

## 2. 已确认的生产代码缺口

以下路径均相对 `src/`：

| 生产位置 | 审计证据 | 实际限制 |
| --- | --- | --- |
| `rust/crates/proletarius-call/src/sdp.rs:57–69` | `AudioCodec` 派生 `Eq/PartialEq`；AMR-WB 保存 `octet_aligned/mode/policy`，EVS 保存 `header_full`；policy 包含 mode-set、period、neighbor | 完整配置相等与“本地支持该新配置”是两种不同判断 |
| `rust/crates/proletariusd/src/native_ims_calls.rs:1873–1889` | `in_dialog_invite` 要求 `current.codec == remote.codec`，否则回复 488 | 新 re-INVITE 不能切换 EVS↔AMR-WB，也不能更新同一 AMR-WB 的集合、period/neighbor、OA/BE 或 EVS 的 hf-only |
| `rust/crates/proletariusd/src/native_ims_calls.rs:1966–1983` | `media_update` 使用同一 equality 门禁 | SDP UPDATE 同样拒绝上述本地可支持的新配置 |
| `rust/crates/proletariusd/src/native_ims_calls/media.rs:456–469` | `Control::Configure` 在已有 worker 的 codec 不相等时返回 `ims_media_codec_change_unsupported` | 只放宽 SIP owner 不足以完成重协商；真实媒体线程仍拒绝 |
| `rust/crates/proletariusd/src/native_ims_calls/media.rs:318–342,357–382` | encode/decode 读 `self.codec` 中固定的 policy 和 framing；CMR 只更新 `requested_mode` | CMR 不更新 SDP policy，也不更新 codec family |
| `rust/crates/proletariusd/src/native_ims_calls.rs:1356–1375` | 本地主动 refresh re-INVITE 的 answer 要求 codec/PT 与当前值相等 | answer 路径也需要保留本地 offer 后进行协议验证，不能只复用 fresh-offer admission |
| `rust/crates/proletariusd/src/native_ims_calls.rs:1548–1574` | offerless re-INVITE 的 ACK answer 也要求 codec/PT 相等 | 当前 200 中只提供当前 codec；ACK 不能擅自选择未 offer 的另一 codec |
| `rust/crates/proletariusd/src/native_vowifi_session.rs:2130–2140` | 注册会话创建真实 `media::Owner` 并交给 `native_ims_calls::Service` | 这些门禁位于实际生产路径，不只是历史 helper |

具体例子：已建立的 AMR-WB 通话从 `{2,8}` 收到合法新 offer `{0,2,8}`，或仅把 `mode-change-period=1` 改为 `2`，两种单独配置都可通过当前 SDP/codec 能力检查，但上述 equality 会拒绝新 offer。`sdp.rs:230–235,307–348,434–475` 分别提供本地能力筛选及完整 AMR-WB policy 解析。这是可实施的剩余软件缺口；不把“协议允许发起修改”误写成“所有终端必须接受任何修改”。

## 3. 必须保留的协议语义

实施前复核以下原始规范；不得通过简单删除 equality 来绕过约束：

1. **新 offer 与 answer 分开验证。** [RFC 3264 §4](https://www.rfc-editor.org/rfc/rfc3264.html#section-4) 的 offer/answer 是原子交换；拒绝后应保持原会话，并处理尚未完成交换的 glare。新 offer 可按本地真实能力选择新 codec；answer 必须属于那一次保留的本地 offer。
2. **动态 PT 映射保持会话一致。** [RFC 3264 §8.3.2](https://www.rfc-editor.org/rfc/rfc3264.html#section-8.3.2) 允许后续 offer 增删格式，但同一媒体流的动态 PT 不能在会话中改为另一个 codec。EVS PT96→AMR-WB PT98 之类的明确映射与把 PT96 改名为 AMR-WB 不同。应维护 dialog 生命周期的已用映射，并为封装配置变化制定无歧义的 PT 规则。
3. **AMR-WB 的 answer 不能随意改变 offer。** [RFC 4867 §8.3.1](https://www.rfc-editor.org/rfc/rfc4867.html#section-8.3.1) 要求 answer 保持该 PT 所 offer 的 octet-align、crc、robust-sorting、interleaving、channels。offer 若明确 mode-set，answer 必须原样保留或拒绝；offer 未声明时，unicast answer 才能选择集合。period/capability 是有方向的接收要求和发送能力，不得简单镜像。
4. **接受新集合后不能继续发送集合外模式。** [RFC 4867 §8.2](https://www.rfc-editor.org/rfc/rfc4867.html#section-8.2) 的 mode-set 同时限制语音帧和 CMR。新集合排除正在发送的模式时，必须定义首个新配置帧的合法模式，不能只替换 policy 后等待下一 CMR。
5. **ACK 事务安全与媒体切换时机分别设计。** 当前代码和测试把 re-INVITE 的持久提交延迟到匹配 ACK，保留此事务屏障、重传和 timeout 安全属性。但不能将“收到 ACK 后才考虑任何新媒体”描述为普遍规范要求：[RFC 3264 §8.3.2](https://www.rfc-editor.org/rfc/rfc3264.html#section-8.3.2) 明确规定 answer 发出/收到时的发送格式约束，并要求对旧接收格式保留短暂容忍。需要明确准备接收、发送切换、ACK 确认及旧接收退休的分别时点。

原子回退不意味着在已经接受新 SDP 后悄悄继续旧格式。若 accepted 之后发生无法恢复的提交失败，必须可观察地结束 dialog 并完成媒体 drain。

## 4. 建议实施边界

### 4.1 Prepare / commit / discard

在当前 `CallBinding`、session generation 和精确 SIP 事务下保留 pending media negotiation，而非直接就地改动活动 worker：

- **Prepare：** 校验真实可用 codec、该 offer/answer 的约束及历史 PT 映射；提前创建必要的新 encoder/decoder。任何分配失败应在回复成功前变成明确拒绝，现有媒体继续工作。准备过程也需要有界队列、取消和退出责任。
- **Commit：** 按明确的信令/媒体过渡阶段，在同一媒体 owner 的串行边界更新 codec、policy、PT、endpoint 和可见状态。每个事务只提交一次，不能由重传 200、重复 ACK 或迟到答复再次执行。
- **Discard：** 对拒绝、过期、glare、旧 generation/call、停止及取消释放所有 staged worker 和资源，等待确定 drain；不得污染当前 codec/CMR。

可扩展现有 `Media`/`Control` 契约，但不能只在 SIP owner 加“允许不同 codec”的布尔值。`media.rs` 的真实编码器更新和资源生命周期必须一并落地。

### 4.2 同 codec 更新

同一 AMR-WB encoder 保留预测状态、decoder 历史及已经运行的帧相位；policy 和 framing 在串行帧边界变更。pending CMR 必须按新集合重新验证或清除。若当前模式在新集合内，不应因相同族重协商而无故跳回初始最高模式；若当前模式被排除，应明确选择首个合法新模式，并证明没有发送一个集合外帧。period/neighbor 与集合更新交叠时的相位、相邻集合定义必须有规范依据和测试。

EVS hf-only 或 AMR-WB OA/BE 更新应以实际 negotiated payload configuration 为准。旧队列不能交给新 framing 解码。必要的短暂旧/new接收能力应有独立状态和明确退休条件，不允许一个 PT 同时产生不可判定的两种解析方式。

### 4.3 跨 codec 更新及 RTP 连续性

EVS↔AMR-WB 可准备新的真实 codec worker，再在边界切换；不要求把不支持跨 profile 切换的原生 C encoder 强行改为可切换对象。必须保留同一 RTP socket/local port、Swift PCM socket/token、SSRC、sequence、timestamp 和当前 16 kHz/mono/20 ms PCM 契约。两种 RTP 时钟均为 16 kHz，20 ms 为 320 ticks。

发送序号、timestamp 不能因创建新 codec 归零；旧 worker 必须确认退出。reorder/sequence、旧来源报文与正在进行的 DTMF 应有明确处理：不能把旧 codec 包排入新 decoder，也不能重复或悬空完成 DTMF。当前 endpoint 变更会重置接收 reorder/sequence 并结束 DTMF（`media.rs:461–466`）；扩展时应检查并保持实际需要的连续性边界。

### 4.4 SIP owner、状态及错误

保留匹配 ACK、200/rejection 重传、491 glare、CSeq/connection/tag 校验、会话刷新超时和 call-generation 栅栏。当前 ACK 提交在 `native_ims_calls.rs:1520–1602`；ACK timeout 发送 BYE 并 drain 在 `:1065–1078`。UPDATE 当前在回复 200 后配置真实媒体（`:2038–2049`），因此引入可能失败的 codec 分配时更需要提前 prepare。

成功提交后更新 `snapshot.codec` 等状态并通知观察者，但不重置既有通话开始时间。当前 codec 可见值仅由 `start_media` 写入（`:685–688`），而此方法也会设置 `started_at`（`:693–696`）；不能直接借重新启动媒体掩盖状态投影问题。

无共同或未验证 codec 继续明确拒绝；不引入 AMR-NB/PCMA、不重复拨号、不把 EVS IO/其他带宽或码率声明成已经支持。真实 codec 失败、commit 失败及清理失败必须带明确错误，不能吞掉错误并维持虚假的 active 状态。

## 5. 必须完成的非平凡测试

只调整 owner 单元测试的 equality 断言不足以验收。现有 owner 的 `FakeMedia::configure` 无条件成功（`native_ims_calls/tests.rs:27–31`）；现有 re-INVITE/UPDATE 主要验证 endpoint 变更（`:1824–1968`），不能证明 codec 切换。真实 media 测试的 endpoint/PT 更新也保持相同 codec（`native_ims_calls/media/tests.rs:125–150`）。

| 层级 | 必测输入/竞争 | 通过证据 |
| --- | --- | --- |
| SDP/协议 | fresh offer 与 retained local offer 的 answer 分别覆盖；显式/省略 mode-set；period/capability 方向；错误 framing；未 offer codec；历史 PT 冲突 | 合法配置准确入选，非法 answer 不扩权，旧会话拒绝后不变 |
| 同一真实 AMR-WB worker | 集合扩大/收窄、当前模式被排除、仅 period/neighbor 变化、pending CMR 途中更新、OA/BE 的有效新配置 | 连续多帧 actual FT/长度符合新集合；保留 encoder/decoder 状态与帧相位；旧 CMR 不越权；首帧即合法 |
| 真实跨 codec | 同一活动 dialog 连续 EVS→AMR-WB→EVS，多个 voiced PCM 帧且双向解码 | 实际 codec/载荷独立解码通过；同 local socket/token/SSRC，sequence 连续、timestamp +320，零意外 decode error，无 worker 泄漏 |
| 过渡接收 | 已排队旧 RTP、乱序/重复/迟到、错误源、旧/new PT/endpoint、切换前后 CMR | 接收容忍满足定义，旧 payload 不进入新 decoder，不因重置 tracker 重放旧请求；旧接收状态按条件退休 |
| SIP 事务 | 带 offer re-INVITE、UPDATE、offerless re-INVITE/ACK、refresh answer；200/ACK/rejection 重传；glare；ACK 缺失或非法 | prepare/commit/discard 次数准确，重复请求不分配新 worker；保留明确 BYE/timeout 行为和正确 SDP version |
| 失败/退出 | prepare 创建失败、提交失败、codec mode setter 失败、stop/lease 丢失与 prepare/ACK 并发、旧 call/generation 迟到 | 接受前失败保留旧媒体；接受后失败可见并结束；staged/active worker 和精确 socket 全部 drain，无旧代副作用 |
| DTMF 与状态 | DTMF 中切换、切换后继续 DTMF、codec 可见状态、started_at、事实记录 | 无重复/悬空任务；状态反映真实 codec，通话时间不重置，终态和错误不丢失 |
| 完整 production public-call fixture | 通过公开控制建立原生 IKE/ESP/IMS/RTP/PCM；在同一呼入和呼出 dialog 做完整 codec/policy 往返及负例 | 对端 SDP/ACK 与真实 worker 一起受测；以收到并成功解码到 host 作为下一发送帧屏障；逐帧核对载荷和 RTP 连续性 |

当前完整 production fixture 的 `amr_wb_cmr_on_the_live_dialog`（`native_vowifi_session/service/production_integration/call_exchange.rs:125–151,331–333`）只是在同一 AMR-WB dialog 中发 RTP CMR，不是上述 SDP 重协商证据。应在其完整真实路径上扩展，不把单独 codec encode/decode 或 FakeMedia 成功作为替代。测试不能在 codec 不可用时 skip 后计作通过。

## 6. 完成口径及继续开放的总目标

本工作包未来需要实现、上述真实路径测试、相关原有回归和独立审阅的实际记录，才能关闭“受支持 codec/policy 的 SDP 重协商”软件缺口。本文仅列证据与行动项，不增加完成计分。

即使软件重协商完成，真实运营商来去电、双向语音和音质、EVS 运营商支持及互通、真实硬件/模块 DSP、正常 LPA、短信、N待多 Profile、自然租期与长稳、P6 旧版回退及受控性能验收等仍须按原总目标独立保留。历史 SIM World 注册证据只对应其具体受测二进制，不外推到新二进制、实网呼叫或媒体重协商。

**结论：P5 媒体重协商未实现；P5、P6 与完整全 Rust 产品验收仍开放。**
