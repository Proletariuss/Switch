# EVS / AMR-WB implementation — 2.86

## Production scope

- Real native EVS Primary WB 13.2 kbit/s encoder/decoder from pinned TS 26.443
  V16.3.0 reference C, linked statically; per-call state, no codec subprocess.
- Prefer EVS (PT96), then AMR-WB (PT98, modes 0..8), telephone-event/16000 (PT102).
- AMR-NB is disabled at offer, answer, incoming-call, and media-start gates. Its
  native adapter was removed. PCMA/PCMU remain disabled. The staged Rust SDP
  policy agrees with production Go; it does not claim to own native RTP media.
- Native-rate 16 kHz PCM throughout the reader audio bridge, including ringback
  before SDP arrives; mixer owns continuous conversion to hardware format.
- EVS compact/header-full single-frame RTP; AMR-WB OA/BE single-frame RTP;
  ptime/maxptime 20 ms. EVS AMR-WB IO wire mode and multichannel are not offered.
- Native PLC; bounded RTP reordering. Playback no longer modifies the decoded
  waveform with a slope limiter or per-packet resampler. Initial 120 ms queue
  can grow to 240 ms after actual underruns; scheduled backlog capped at 400 ms.
- Stop the audio engine before switching voice processing; restore unprocessed
  input if the system unit rejects configuration.

## Validation

- 50 EVS frames encode/decode byte-identical to independent reference CLIs.
- AMR-WB 12.65 reference encode/decode byte-identical; all nine WB modes checked.
- Production UDP workers exercised with real loopback sockets in both directions
  for EVS compact/header-full and AMR-WB OA/BE, 640-byte PCM and 320-tick steps.
- Early-media token handshake works without microphone audio.
- Native constructor failure prevents advertising unusable media; probes close.
- Race tests for imscore and macOS backend passed; Rust call tests and Go/Rust
  SDP parity passed. No outbound SMS tests were performed.
- EVS 20 ms encode+decode benchmark: ~0.115 ms/frame on Apple M4 (silence input;
  performance check, not a speech-quality or formal standards-conformance claim).

## Real carrier evidence before final playback changes

SIMworld/1033 selected AMR-WB / 16 kHz after the EVS-first offer. One observed
call sent 2064 RTP packets and received 2200 with zero decoder errors. User
reported noise/stutter despite these counters. This run is NOT accepted as a
successful quality test. EVS was verified locally but not selected by the carrier
on this call. Final native-rate playback and buffering changes require another
listening test. No claim that EVS is guaranteed for every destination.

### 无声回归跟进

用户反馈原采样率直出版本无声音，撤回播放器 16 kHz 单声道图配置，恢复 48 kHz 双声道播放图。保留 16 kHz 输入及 EVS/AMR-WB 限制，使用持久 AVAudioConverter，无下行非线性整形。连续 500 帧转换检查通过：480000 输出采样、双声道一致、RMS 0.1414。新增有界 audio-diagnostics.log，仅记录接收/接受/拒绝/播放帧计数与引擎状态，不含音频内容、号码或令牌。构建成功，实机听感尚未验收。

### 接通后播放队列停滞修复

实机日志：收到 1450 帧、接受 1450 帧，播放完成停在 184 帧，队列恒为 20 帧，后续新帧被上限丢弃。引擎因麦克风/设备切换而停止后，旧调度可能被废弃，但计数未清空。修复所有引擎重启路径：先递增播放代际并停止播放器，清空旧排队计数/预缓冲，重置转换器，重新连接播放图，重新接收并预缓冲新帧；过时代际回调不能改变新队列。已开启语音处理时避免重复切换。使用实际 ReaderVoWiFiAudioBridge 源码的离线 AVAudioEngine 渲染检查通过：首次启动及连续两次停止/重连均有非零 PCM 输出。实机听感待验收。

实机复测：新版已连接 SIMworld 并拨打已授权 1033，AMR-WB/16 kHz，RTP 解码错误 0。麦克风切换发生一次队列重置；接通后播放完成计数 199 → 449 → 699 → 950 持续增加，未再停滞于固定值。尚待用户确认扬声器实际听感，不能仅凭播放回调判定音质修复。

用户实机验收：本次 1033 测试明确反馈“有声音且清晰”。实际协商 AMR-WB；不将本次验收记作运营商 EVS 通话验证。测试结束后界面为空闲，未发送短信，未生成 DMG。

### EVS-only 实机互通测试

用户要求尝试仅 EVS。隔离测试包 build/evs/Switch-EVS-only.app，通过链接参数开启 evsOnlyInteropTest=1；默认生产策略不变。SDP 仅提供 EVS WB 13.2（PT96）及 telephone-event（PT102），禁止 AMR-WB 回退。单测确认无 AMR 广告且拒绝 AMR-WB 答复。SIMworld 已注册，呼叫 1033 返回 SIP 488 Not Acceptable Here，用户同步确认此错误。结论限于当前线路/目的地/所提供 EVS 参数，不推断运营商全网不支持 EVS。退出测试包，恢复已验证的 EVS 优先、AMR-WB 兼容版本；不生成新的 DMG。

### 国际拨号前缀兼容

统一拨号入口将纯数字国际号码前缀 00 转换为 +，支持空格、短横线和括号；覆盖模块、读卡器 VoWiFi、历史回拨。前缀按钮避免生成 +00。IMS URI 构造同步支持 00，避免错误附加本地 phone-context。保持短号、单零开头本地号码、服务码以及明确 SIP/tel URI 的原行为。Go 号码转换测试和 Swift DeviceCallCorrectness 检查通过；未拨打真实国际号码。
