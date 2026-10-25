# Iteration 416：远端媒体 SDP 协商移除 Go fallback

非空远端 SDP（offer 和 answer）现在必须经 `NegotiateAudioSDP` Rust callback。Go `applyRemoteSDP` 中原有 SDP parser、EVS/AMR-WB payload/parameter 选择、telephone-event 选择和重复参数判定已删除；相关 Go helper 也移除。缺少 Rust negotiator 时明确失败，Rust 错误或 endpoint/codec/capability 输出不合法时不会提交新的 RTP/媒体状态。最终响应没有 SDP 时，只有在可靠早期媒体已经成功协商 EVS 或 AMR-WB 的情况下才复用此前结果。

生产 Go IMS session 已配置该 Rust callback。Go 仍做 codec 能力探测、校验 Rust 返回结构并投影到本地运行态，还负责 SIP I/O、媒体执行和 codec 实例生命周期；Go `localSDPLegacy` 本机 SDP builder 是后续独立迁移项，本轮不计为已完成。

测试覆盖 Rust negotiator 缺失失败关闭且媒体状态不变、Rust 返回拒绝时不覆盖先前媒体协商、PRACK/UPDATE 活动媒体不被候选协商污染、Rust fixture 输出与边界适配投影 parity。全量 Rust workspace、Go 全包测试及 vet、Swift BLE 回归、Rust/Go codec parity、完整迁移门禁通过。IMS 呼叫包 `go test -race ./internal/vowifi/imscore` 通过。API contract census 保持 346 method/path（Rust 286 / Go 60）、127 Swift 调用点和 370 reviewed scopes；无新增公共 API。

门禁日志：[iteration-416-migration-gates.log](../build/optimization/iteration-416-migration-gates.log)，SHA-256 `1ce9d952c36c48d4f00e5913f6bd1f221e14bb5ad7a0a51b7401730c805292f0`。定向 race 检查通过；无运营商实机呼叫/音频验收。

P3 +0.025 至 **2.890 / 8.0**；整体 **17.595 / 47.5 = 37.04%**（估算误差约 ±3 个百分点）。退出审计仍报告 7 类阻塞，其中 264 个生产 Go 源文件尚存；本轮只收敛远端 SDP 决策，不代表后端已脱离 Go。本轮未生成 DMG；最近的 3.08/build308 Iteration 415 DMG 不含本轮源码。
