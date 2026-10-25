# Iteration 302：Rust RTP 有界重排窗口

在 `proletarius-call` 增加 `RtpReorder` 和拥有 payload 的 `OwnedRtpPacket`。它镜像现有 downlink 策略：SSRC变化时重置队列；按16位回绕差丢弃重复/迟到包；复制payload以脱离UDP接收缓冲生命周期；最多暂存32个ahead packet；40ms仍有序号缺口时选取最近的前向包，以便上层执行丢包隐藏。提供下一个缺口deadline供后续Rust媒体worker做等待，不自行运行socket或时钟任务。

验证：序号回绕/补包/40ms缺口/重复包/SSRC变化/有界队列两项专项测试通过；`proletarius-call` **26 passed**。完整迁移门禁通过：Rust workspace **1189 passed / 3 ignored**；Go vet/全仓兼容测试及载波2722案例、USB/radio/HTTP/SDP/AMR/MESSAGE parity通过。Go 1.26.3下载因TLS timeout，使用本机Go 1.26.7。格式检查与 `git diff --check` 通过。该组件尚未连接线上媒体socket、decoder或CoreAudio，也没有替换Go `rtpReorder`，因此不代表生产RTP owner迁移，不计总体加权进度，仍为 **31.4%**。无实机通话或DMG。
