# Iteration 419：DTMF 事件序列由 Rust 生成

本轮将生产 RFC 4733 DTMF 事件帧序列计算接入 Rust。Rust 根据按键与采样时钟生成持续时间递增及结束标记；Go 通过本地 Rust IPC 获取完整序列，继续按原 RTP 节奏发送。Rust 调用或返回值不可用时失败关闭，Go 不再保留生产事件序列生成副本。

完整迁移门禁通过：[iteration419-migration-gates.log](../build/optimization/iteration419-migration-gates.log)，SHA-256 `f864157018c3edf9fa4577f22f9d1e7358aaa112521e85acb026e4c5b527a40f`。Rust workspace、Go 全仓 test/vet、Swift BLE proof、载波/USB/射频/HTTP/codec/SDP/AMR/MESSAGE parity 及迁移契约检查通过；DTMF Rust/Go bridge、UDP 帧序列、失败关闭和 race 定向测试通过。契约清单仍为 346 method/path（Rust 286 / Go 60）、127 个 Swift 调用点、370 个审阅范围。

本轮保守计入 P5 +0.025 计划人周等值：累计 **0.250 / 8.0**；总计 **17.670 / 47.5 = 37.20%**，估值误差约 ±3 个百分点。全 Rust 退出仍受 7 类 Go 构建、运行或源码依赖阻断；当前仍有 264 个生产 Go 源文件。没有执行真实读卡器、SIM、通话或运营商测试。

按用户要求生成 [Switch 3.09/build309 Iteration 419 DMG](release-3.09-iteration419.md)。该包为本机 ad-hoc 签名，未公证；旧的无迭代后缀 DMG 保持不变。
