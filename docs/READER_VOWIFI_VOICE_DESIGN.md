# Proletarius 读卡器 VoWiFi 语音设计（0.02）

状态：三轮交叉审阅通过，按本文分层构造与验证。

## 不可违反的边界

- ePDG 仍由前置代理/fake-IP 接管。应用不绑定物理网卡地址，也不自行解析或替换 ePDG 地址。
- SIP、RTP、RTCP 复用同一 SWu 用户态网络栈和同一 ePDG 会话。不得创建第二个 `InnerPackets` 消费者。
- SIP 使用已经协商的 ipsec-3gpp port-c/port-s；RTP/RTCP 由外层 SWu Child SA 保护。
- 读卡器语音和 DJI USB 音频是两条独立设备链路，任何改动不得改变 DJI AT/USB 音频行为。

## 呼叫状态机

`idle → outgoing/incoming → early/ringing → connecting → active → terminating → ended/failed`

- 去电：INVITE，处理 100/180/183；可靠临时响应发送 PRACK；200 后发送 ACK。早期取消使用 CANCEL，已建立通话使用 BYE。
- 来电：INVITE 先回 100，再回 180；用户接听才回带 SDP 的 200，拒接回 486。CANCEL 回 200 且原 INVITE 回 487；BYE 回 200。
- 支持 UPDATE/re-INVITE、Session-Expires/Min-SE 和 422 重试；profile 切换或拔卡会终止并清空当前代次。
- MESSAGE 和 OPTIONS 可立即回 200；ACK 不得回响应；未知方法回 405。

## 媒体

- 首选 AMR-WB，其次 AMR-NB，20 ms 帧；支持 RFC 4867 octet-aligned、动态 payload type 与 telephone-event。
- RTP/RTCP UDP 端点绑定隧道内层 IP；SDP 远端只能是数值 IPv4/IPv6，不进行 DNS 查询。
- 初始抖动缓冲 60 ms；队列有界并丢弃最旧帧；按运营商模板执行 RTP 不活跃超时和 RTCP 周期。
- macOS 使用 AudioToolbox/AVAudioEngine 编解码和采集播放；只提供 EVS 时明确报告不支持，不伪装成功。

## 本地接口与 UI

- 后端提供 status/dial/answer/reject/hangup/dtmf；每个请求携带 session generation。
- 音频经权限为 0600、带随机会话令牌的本地 Unix 数据报桥传输，控制事件与媒体事件分离。
- 通话页保留 DJI 设备，并增加以当前号码命名的 Reader 设备；支持拨号、接听、拒接、挂断、静音、扬声器和音频设备选择。
- E911/紧急呼叫在定位与运营商 entitlement 未完整实现前禁止发起。

## 三轮审阅结论

1. 协议审阅：删除全方法自动 200，补齐 100rel/PRACK、预条件、早期媒体、CANCEL/BYE 和会话刷新。
2. 架构审阅：共享唯一 netstack；呼叫控制采用串行事件循环；音频有界队列不阻塞 SIP/IKE/SMS。
3. 可运行性审阅：只接受隧道/运营商下发地址；编解码能力运行时验证；拔卡/切卡强制回收；不支持项失败可见。

## 发布门禁

- Go 全量测试、Swift 严格构建、SIP 黄金流程、RFC 4867 向量、UDP 数据面、并发 SMS/通话、拔卡/切卡均通过。
- 真机至少完成一组去电、一组来电、双向音频、DTMF 和通话中短信。未经真机验证不得标记为“可用”。
