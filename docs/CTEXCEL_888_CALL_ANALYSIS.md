# CTExcel 888 IMS 呼叫失败分析

> 记录日期：2026-08-30  
> 0.09 已完成短号码规范化、487 分类、早期媒体和本地回铃修复。

## 现象

使用 CTExcel UK Profile 通过读卡器 VoWiFi 拨打客服短号 `888`，应用显示：

```text
呼叫失败：IMS call rejected: SIP 487 Request Terminated
```

## 已确认事实

1. IKEv2、EAP-AKA、ESP 隧道均已建立。
2. IMS REGISTER 完成 401 AKA challenge 后返回 200，IPsec-3GPP 安全通道已启用。
3. CTExcel 官方确认 `888` 是 CTExcel 用户可免费拨打的客服短号。
4. 多次拨号均在 INVITE 发出后迅速收到 487，可稳定复现。
5. 本端未进入 `cancelling` 状态，因此不是应用主动发送 CANCEL 后的正常 487 收尾。
6. 当前 Request-URI 和 To 均使用裸本地号码：`tel:888`。
7. 当前代码只给没有 `sip:`/`tel:` 前缀的目标补上 `tel:`，没有识别本地短号，也没有添加 `phone-context`。
8. 当前 SDP 只宣告 PCMA/PCMU；代码和测试明确排除了 AMR/AMR-WB。
9. 同一运行环境中拨打普通国际号码曾收到运营商 `400 Bad Request` 和 `Incompatible media format`，证明现有媒体能力已经造成实际互通失败。

## 根因判断

`888` 不是 E.164 全球号码，而是运营商本地服务号码。RFC 3966 要求本地 `tel:` URI 必须携带 `phone-context`；3GPP TS 24.229 对 IMS 本地号码也要求根据归属网络域添加该参数。

当前生成的：

```text
INVITE tel:888 SIP/2.0
To: <tel:888>
```

缺少本地号码的路由上下文。CTExcel 使用 234/33 归属标识，但 IMS challenge、S-CSCF/TAS 路由位于 EE 的 `mnc030` 核心网，因此裸短号到达 IMS 后无法可靠映射至 CTExcel 客服业务。

此外，GSMA IR.92 要求 IMS 语音终端支持并在初始 SDP 中提供 AMR 和 AMR-WB。当前实现为了避免“信令接通但无声”而只宣告 Mac 音频桥已经实现的 G.711，这种做法虽然是失败关闭，但不满足 CTExcel/EE IMS 的媒体互通要求。客服短号通常还需要接入运营商媒体资源/语音播报平台，更可能拒绝没有 AMR/AMR-WB 的呼叫。

本次 487 没有携带 `Reason` 或 `Warning`，因此仅凭终端侧日志不能证明是短号路由还是媒体资源平台单独触发了终止。准确结论是：

- 直接原因：运营商网络终止了 INVITE 分支；不是本端主动取消。
- 已确认信令缺陷：`tel:888` 缺少 `phone-context`。
- 已确认媒体缺陷：SDP 未提供 IMS 必需的 AMR/AMR-WB，且已经导致普通号码媒体格式拒绝。
- 两项都需要修复，不能只把 487 文案改掉或盲目重试。

## 建议修复

1. 增加统一的 IMS 拨号目标规范化器，区分：
   - `+` 开头的 E.164 全球号码；
   - 本国号码；
   - 运营商服务短号；
   - 紧急号码和补充业务号码。
2. 对 CTExcel `888` 生成带归属网络上下文的本地 tel URI，而不是裸 `tel:888`。
3. `phone-context` 应成为运营商 Profile 的显式字段，优先使用经过注册挑战/运营商模板确认的归属 IMS 网络域，不在呼叫时临时猜测。
4. Request-URI 和 To 必须使用同一规范化目标；CANCEL、ACK、PRACK 应继续保持原事务目标。
5. 实现 AMR-NB 和 AMR-WB 的完整 RTP 编解码、SDP `rtpmap`/`fmtp`、模式集协商和 CMR 处理；在真正具备编解码能力前不得只宣告而不实现。
6. 优先顺序建议为 AMR-WB、AMR、PCMA/PCMU，运营商 Profile 可以收紧但不能声明运行时不支持的编码。
7. 错误提示区分：
   - 本端取消产生的 487：显示“呼叫已取消”；
   - 网络返回的 487：显示“运营商终止了呼叫”；
   - 本地短号缺少上下文：在发送前阻止并提示模板不完整。
8. 增加 CTExcel `888`、普通英国号码、国际 E.164 号码、紧急号码、AMR offer/answer 和真实 RTP 往返的测试。

## 次要问题

SIP TCP 响应存在跨多个 TCP chunk 到达的情况。流式解析器最终能够组装完整响应，但当前会把中间状态记录为 `SIP partial data` 警告。该警告不导致本次 487，但应降级或在确认数据损坏时才输出，避免误导诊断。

## 0.09 实施结果

- 裸短号码 `888` 现在生成 `tel:888;phone-context=<归属 IMS 域>`。
- E.164 号码保持全球 `tel:+...` 形式；Request-URI 与 To 共用同一规范化结果。
- 用户主动取消导致的 487 作为正常结束；运营商返回的 487 明确显示为“运营商终止了呼叫”。
- 180 或无可用 SDP 的 183 播放本地英式回铃；具有可用 G.711 SDP 的 183 播放运营商早期媒体。
- 未获得最终 2xx 前不采集麦克风；接通后才启动上行，并显示 Mac 当前输入/输出设备。
- 已增加短号码、E.164 和网络 487 分类测试，后端全量测试通过。

AMR/AMR-WB 仍是一项明确的媒体能力缺口。0.09 不宣告未实现的编解码，因此不会把 AMR-only 会话伪装成“已接通”。

## 0.10 真机证据与修正

0.09 真机请求已正确带上 `phone-context`，但使用了 CTExcel 用户归属域 `ims.mnc033.mcc234.3gppnetwork.org`。运营商返回：

```text
SIP/2.0 487 Request Terminated
Reason: Q.850;cause=127;text="Interworking"
```

同一会话的 IMS AKA challenge realm 是 `ims.mnc030.mcc234.3gppnetwork.org`。这证明 CTExcel 的身份/归属 PLMN 234/33 与承载 MMTel 业务的 EE 234/30 核心域必须分开建模。

0.10 增加独立的 `ims_phone_context`：

- CTExcel 登记和用户身份仍保持 234/33，不破坏已验证的 IMS REGISTER/SMS。
- `888` 等本地业务号码改用 `ims.mnc030.mcc234.3gppnetwork.org` 进入 EE MMTel 核心。
- 自定义运营商覆盖可单独设置 `ims_phone_context`，不再被迫修改 IMS 身份域。
- Q.850 127 现在会保留在错误提示中，不再只显示笼统的 SIP 487。

## 0.10 真机复核与更正

0.10 已按上述假设把 `888` 改为 `phone-context=ims.mnc030...`，但真机仍在
`100 Trying` 后收到完全相同的 `SIP 487 / Q.850 cause=127 Interworking`。
因此 `mnc030` 只证明了认证挑战所在的宿主核心，不能替代 IR.92 对本地号码
的明确规则：`phone-context` 必须与 SIP REGISTER Request-URI 使用的归属域一致，
本卡即 `ims.mnc033.mcc234.3gppnetwork.org`。

同一张 CTExcel 卡拨打全球号码时，网络返回的 SDP 进一步给出了决定性证据：

```text
m=audio ... RTP/AVP 97 101
a=rtpmap:97 AMR/8000
a=fmtp:97 octet-align=1;mode-set=7;max-red=0
```

此前实现只宣告并实现 PCMA/PCMU。普通被叫可以先进入无媒体振铃，但 `888`
客服平台依赖 AMR 早期媒体/语音资源，因没有可互通的语音编码而直接终止。
0.11 因此恢复规范要求的 `mnc033` phone-context，并加入真实 AMR-NB 12.2 kbit/s
编解码和 RFC 4867 octet-aligned RTP；不会只修改 SDP 而留下接通后无声。

## 0.11 真机结果与 0.12 崩溃修复

0.11 两次拨打 `888` 均已获得 `SIP 200 OK`，网络返回 AMR mode 7 的有效
RTP 端点。这证明本地短号路由和 AMR 协商已经打通。接通后的闪退并非 IMS
拒绝，而是 macOS 前端把 8 kHz 单声道 PCM buffer 直接调度到通常为 48 kHz
立体声的 `AVAudioPlayerNode` 连接，AVFAudio 以 Objective-C 异常终止进程。

0.12 将下行 PCM 明确重采样至当前系统输出的采样率和声道数，播放节点、
回铃节点和所有调度 buffer 使用完全相同的格式；音频启停与收包调度也统一
串行化，避免断开/重连时 detach 与 schedule 并发。SIP TCP 的正常分片不再
被误报为解析失败。

## 0.14 语音质量、抗干扰、AirPods 与通话按键

- 播放端加入 60 ms 预缓冲，发生真实欠载后重新蓄积，避免把迟到帧逐帧播放成断续杂音。
- RTP 下行丢弃重复和迟到包；连续丢失不超过三帧时，以逐级衰减的上一帧做短时丢包隐藏。
- Mac 麦克风启用系统支持的回声消除和噪声抑制，并增加直流偏置去除、自适应噪声门和峰值限制。
- 下行增加直流抑制与异常跳变限制，降低损坏帧产生的爆音，同时保留正常语音动态。
- 通话接通后可展开按键盘。读卡器 VoWiFi 使用 SDP 协商的 RFC 4733 `telephone-event`，DJI 模块使用其语音栈 DTMF 指令。
- 播放节点固定连接到稳定的 48 kHz 双声道内部混音格式，由 macOS 转换到当前硬件，避免 AirPods 从 A2DP 切换到通话模式时改变采样率/声道而产生杂音或崩溃。
- 监听系统音频配置变化；通话中切换 AirPods、内置设备、有线耳机或 USB 声卡时自动重建麦克风转换器并重启音频图，失败会限次重试并明确提示。
- 输入和输出分别跟随 macOS 的系统默认设备，因此支持 AirPods 输出配 Mac 麦克风等混合路由；通话页同步展示当前实际设备名称。
