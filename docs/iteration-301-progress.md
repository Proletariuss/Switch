# Iteration 301：Rust EVS codec 与单帧 RTP 格式

在 `proletarius-call` 增加 Rust EVS Primary WB 13.2 / AMR-WB native codec wrapper。每个实例独立持有 reference C encoder/decoder state；Rust API 校验20ms、320-sample PCM帧，限制bitstream尺寸，验证Primary/AMR-WB帧长度，并支持丢帧PLC。unsafe仅在 crate 禁令下按 FFI 声明和 codec 调用项显式放行。macOS arm64 有 Switch/build 中的 reference archive 时启用；archive 缺失时 codec 构造失败关闭，不影响纯协议代码构建。

Rust同时实现单帧 EVS Primary compact/full ToC packetize/depacketize、可选CMR及严格 ToC/长度校验。测试黄金向量复制到 crate 的 `testdata/evs`，Rust回归不再依赖 Go package目录；逐帧对3GPP参考编码/解码结果校验50帧，并检查AMR-WB mode 7、PLC、错误帧及ToC格式。

验证：完整 Rust workspace 测试 **1189 passed / 3 ignored**；本轮修改后的 `proletarius-call` **24 passed**；完整 `scripts/test-migration-gates.sh` 通过，含 Go vet/全仓兼容测试、载波2722例及 USB/radio/HTTP/SDP/AMR/MESSAGE parity；contract census 334条（Rust273/Go61），`cargo fmt --check` 与 `git diff --check` 通过。Go 1.26.3 下载遇TLS timeout，使用本机 Go 1.26.7（高于 go.mod 最低版1.26.3）运行离线门禁。

本轮只把 codec/单帧wire组件带入 Rust，生产 SDP negotiation、call/media actor、RTP收发、DTMF媒体、CoreAudio桥和完整通话生命周期仍归 Go/Swift；没有真实呼叫或设备验证。故不计生产owner加权进度，整体保持 **31.4%**。结构退出审计仍阻塞于7类Go构建、监督、路由及源码依赖；未访问读卡器、未拨号/发信、未生成DMG。详情见 `docs/optimization/all-rust/runtime-capability-inventory.md` 的 RT-G02。
