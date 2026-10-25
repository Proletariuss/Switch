# Iteration 418：codec 能力探测移交 Rust

生产远端 SDP 协商不再接受 Go 传来的 `evs_available` 或 AMR-WB mode 列表。Go IPC 只发送 SDP、offer/answer 标记和 EVS-only 互操作开关；Rust 在自身进程内实例化已链接的 EVS/AMR-WB codec，探测实际可用能力，再由 Rust SDP 协商选择 EVS 优先、AMR-WB 回退。EVS-only 模式仅由 Rust 屏蔽 AMR-WB。Go 仍校验 Rust 返回的 endpoint/codec 字段，并在更新呼叫状态前创建一次最终选择的 codec，保留本机媒体运行就绪检查；Go codec/RTP/PCM 生命周期尚未迁移。

完整迁移门禁通过：Rust workspace 格式/测试、Go 全包 test/vet、Swift BLE proof 与 manager regressions、carrier/USB/radio/HTTP/SDP/AMR/MESSAGE parity 均通过。IMS 呼叫包 `go test -race ./internal/vowifi/imscore`、新增 Go↔Rust SDP IPC schema 回归通过。契约扫描器 44 项测试、inventory `--diff` 通过；346 method/path（Rust 286 / Go 60）、127 Swift 调用点、370 reviewed scopes 保持不变。

门禁日志：[iteration418-migration-gates.log](../build/optimization/iteration418-migration-gates.log)。本轮没有真实呼叫/运营商网络验收。之后按用户要求从 Iteration 418 源码生成 [Switch 3.09/build309 DMG](release-3.09-iteration418.md)，不覆盖既有发行包。

仅计入 SDP 实际 codec 能力探测及由该能力决定的生产协商职责：P3 +0.025 至 **2.940 / 8.0（36.75%）**；项目累计 **17.645 / 47.5（37.15%）**，估值误差约 ±3 个百分点。结构退出审计仍有 7 类 Go 构建/运行/源码阻塞、264 个生产 Go 源文件。本轮不代表 Rust 已拥有 codec 实时编码/解码或 SIP/media session。
