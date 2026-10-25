# Iteration 420：Rust 生成完整 RFC 4733 RTP 数据报

生产 VoWiFi 通话的 DTMF 路径现在由 Rust 根据按键、采样时钟、协商的 telephone-event payload type，以及当前 RTP sequence/timestamp/SSRC，一次生成完整的 7 个 RTP 数据报和后续计数器。Go 在发送前校验 RTP 头、序号、时间戳、SSRC、marker、事件载荷、duration 和结束标志；随后仍使用现有 UDP socket，以 20 ms 间隔发送，并管理取消、音频暂停及通话生命周期。没有把每个媒体帧改为 Rust IPC。

完整迁移门禁通过：[iteration420-migration-gates.log](../build/optimization/iteration420-migration-gates.log)，SHA-256 `0fb82915285355f5cd118a83235512f447ab1c562c16b93fd8dfd3aeb50e95c7`。Rust workspace、Go 全包 test/vet、Swift BLE 回归和载波/USB/射频/HTTP/codec/SDP/AMR/MESSAGE parity 通过；Rust/Go 定向 DTMF 测试覆盖计数器回绕、RTP datagram 字节、真实 localhost UDP 发送、损坏 Rust 输出失败关闭及 race。迁移契约 `--diff` 通过：346 method/path（Rust 286 / Go 60）、127 个 Swift HTTP 调用点、370 个审阅范围。

本轮保守计入 P5 +0.025 计划人周等值：累计 **0.275 / 8.0**；全项目 **17.695 / 47.5 = 37.25%**，工程估值误差约 ±3 个百分点。剩余 7 类 Go 构建、运行或源码依赖尚未退出；语音会话、codec 实例、RTP socket、实时音频与呼叫生命周期仍未迁移。没有操作读卡器、SIM、短信或运营商网络，也没有实机通话验收；没有生成 DMG。当前 DMG 仍为用户上一轮要求的 3.09/build309 Iteration 419 包，不包含本轮变更。
