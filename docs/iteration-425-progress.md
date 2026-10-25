# Iteration 425：来电终止响应策略迁入 Rust

日期：2026-10-12。Rust incoming-call policy 现在为有效、已跟踪的 CANCEL/BYE 返回对应 SIP 响应状态码和 reason phrase：接受 CANCEL 后返回 CANCEL `200 OK` 并指定原 INVITE `487 Request Terminated`；拒绝 CANCEL/BYE 返回 `481 Call/Transaction Does Not Exist`；已接受 BYE 返回 `200 OK`；本地在振铃阶段拒接返回 `486 Busy Here`。

Go policy bridge 对 action、phase、request CSeq 与响应元组作精确校验；其余 incoming policy action 必须没有响应元数据。Go 继续构造 SIP 响应、保留 Via/To-tag 行为、经现有 responder 写入，并负责 Go-owned call/session 资源清理。未知或错配策略不会产生网络响应。无新路由或传输能力。

本轮另修复 `TestNativeUSSDContentDecoderBridge` 的 Unix socket 路径过长问题：测试改用短临时目录，解决 macOS `sockaddr_un` 长度上限造成的全包测试失败。

验证：Rust call crate 定向测试、Rust daemon 16项来电相关测试、Go IMS/macOS bridge 全包测试、`go test -race ./internal/vowifi/imscore ./cmd/proletarius-macos`、44项合同扫描器自测和全量 `scripts/test-migration-gates.sh` 均通过。Rust workspace 1,425 passed / 3 ignored；合同 census 346 routes（Rust286 / Go60）、127 Swift call sites、370 reviewed scopes。门禁日志：[iteration425-migration-gates.log](../build/optimization/iteration425-migration-gates.log)，SHA-256 `e0b5b35da0ce415483a35531d706aa700d6c64f337985d7a6bf625162e601ade`。

迁移估值 P5 +0.025，累计0.400/8.0；全项目17.820/47.5 = **37.52%**（估算误差约±3个百分点）。静态退出审计仍有7类 Go 构建/运行/源码依赖及264个生产 Go 源文件。未做 IMS/运营商实机验收。本轮未生成 DMG；最新发行包仍为3.10/build310 Iteration424。
