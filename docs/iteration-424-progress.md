# Iteration 424：来电本地挂断策略与 CSeq 迁入 Rust

日期：2026-10-12。来电本地挂断现在必须经过 Rust `IncomingCallTracker`：振铃阶段返回拒接决策，已建立对话返回 BYE 决策和 Rust 分配的 CSeq，结束或不匹配事务则忽略/失败关闭。Go bridge 校验返回的 phase、action 和 CSeq；Go runtime 只负责按策略构造并发送 SIP、确认对话身份及清理媒体/会话资源。

为保证生成的来电 BYE 属于原对话，本轮保存并复用入站 INVITE 的本地/远端 identity URI、tag、Contact 远端目标及 Record-Route。迟到 ACK 在本地挂断后不会让 Rust tracker 恢复成活动状态。网络 I/O、SIP wire 执行和通话生命周期仍有 Go owner。

验证：`scripts/test-migration-gates.sh` 全部通过；Rust workspace 1,425 passed / 3 ignored，Go 全包 test/vet 与现有 parity 回归通过；契约扫描器 44 项通过，API census 为346 method/path（Rust286 / Go60）、127 Swift 调用点、370 reviewed scopes。`go test -race ./internal/vowifi/imscore ./cmd/proletarius-macos` 和 `git diff --check` 通过。门禁日志：[iteration424-migration-gates.log](../build/optimization/iteration424-migration-gates.log)，SHA-256 `c995711a00783e33258a68cb4e1beb83fec07526a95f19d66a56032d46c15707`。

迁移估值 P5 +0.025，累计0.375/8.0；全项目17.795/47.5 = **37.46%**（约±3个百分点）。退出审计仍有7类 Go 构建/运行/源码依赖及264个生产 Go 源文件。没有运营商、读卡器或实机呼叫验收；9eSIM `6982` 仍排除。

阶段发行包：Switch 3.10 / build 310，DMG 于本轮按用户要求生成；见[发行记录](release-3.10-iteration424.md)。
