# Iteration 391：读卡器 VoWiFi 生产呼叫的 SDP 协商迁入 Rust

读卡器 VoWiFi 生产会话现在把来电/去电的远端 SDP 交给 Rust 解析。Rust 负责校验音频 RTP 地址和端口、按 EVS 优先及 AMR-WB 次选选择编码、校验动态 payload type，并选择匹配采样率的 telephone-event。Go 只探测本机编解码器及 AMR-WB 可用模式，把能力事实传给 Rust；生产配置存在 Rust 回调时不会走旧 Go SDP 解析器。

Go 仍管理 SIP 对话、RTP socket、媒体线程、音频 I/O 及实际编解码器实例。因此这是 P5 的局部业务决策接管，不代表语音、VoWiFi 或通话生命周期已经全 Rust 化；没有操作实体卡、拨打电话或验证运营商语音，也没有生成 DMG。

验证通过：Rust `proletarius-call` SDP 单测、daemon call-codec 测试、Go imscore/读卡器后台测试、Rust/Go SDP 差分及完整 `scripts/test-migration-gates.sh`。日志：[iteration-391-migration-gates.log](../build/optimization/iteration-391-migration-gates.log)，SHA256 `212bfd0ee21128b9d09a23043467fd66d31da164b4ce2744bb85579ff752d417`。契约清单 `--diff` 与 `git diff --check` 通过；新增一个 Rust-owned POST method/path，清单为 344 项（Rust 284 / Go 60）、127 个 Swift 调用点、359 个审阅范围。

P5 增加 **0.025 计划人周等值**，累计 **0.025 / 8.0（0.3%）**；项目累计 **16.970 / 47.5 = 35.7263%**，显示 **35.7%**（两位小数 **35.73%**，估算误差约 ±3 个百分点）。
