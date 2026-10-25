# Iteration 100 — SIM World 通话失败归因与 RTP 可观测性

日期：2026-10-07。用户反馈外呼无声音、目标手机未收到来电，随后呼叫以 SIP 487 `Request Terminated` 失败。纠正此前将 SIP 接通/SDP 协商误报为双向音频建立的结论。本轮没有重发短信或再次拨号。

## 修复

- 修复本机 VoWiFi 音频桥启动时的令牌握手：后端现在会先登记回传端点，再等待 PCM。原实现把长度恰好等于令牌的首个握手包当成空音频丢弃；没有麦克风采集的早期媒体阶段因此无法把远端音频送回本机。
- 通话状态新增成功发出的 RTP 包、收到的远端 RTP 包、AMR 解码异常次数及送入本机音频桥的 PCM 帧数。状态页明确区分 SDP 参数协商、本机音频设备启动和实际 RTP/PCM 观察，不再将它们合并称作“双向音频已建立”。
- SIP 487 诊断不再无证据地归因“运营商终止”。`SIP;cause=487` 只说明 INVITE 事务被终止；若无 Q.850 原因或事务侧的本地 CANCEL 记录，不能从该 Reason 单独推断谁取消或为何失败。

## 验证

- `go test -race ./internal/vowifi/imscore ./cmd/proletarius-macos ./internal/vowifi/carrier ./internal/vowifi/policy` 通过。
- `sh scripts/test-device-call-correctness.sh` 通过，覆盖旧状态 JSON 兼容及 RTP 指标解码。
- 新 Go 回归在 loopback 验证：令牌握手先于麦克风 PCM 时，随后到达的 AMR RTP 仍可送达本机音频端点；原双向 AMR worker 测试同时校验计数器。
- Xcode Debug 构建成功：`Switch/build/DerivedDataCallFix/Build/Products/Debug/Switch.app`。未生成 DMG。
- 重启后 UI 与 macOS 均未枚举到外接读卡器（设备显示 `0/2` 在线；系统 USB/PCSC 仅报告内置 SD 读卡器）。未进行实体 SIM 注册、外呼或来电验证；不把此前旧版本的 SIP 200/通话计时作为语音成功证据。

## 仍待实机验证

读卡器重新连接后，需要重新核验一次 1033 外呼的 RTP 上下行计数与实际听音；另需从第二部电话拨入 SIM World 号码，确认 Port-S 收到 INVITE、Switch 弹出通知、接听后双向音频及挂断。当前日志中的 Port-S accept count 为 0，尚无本机收到入站 INVITE 的证据。此前发往用户指定号码的测试短信结果仍为未知，按既有规则不重发。

全 Rust 后端目标估算维持 **24.9%（11.825 / 47.5 人周，约 ±3 个百分点）**；本轮修复和新增观测不改变已迁移业务比例。DMG 按用户要求暂不生成。
