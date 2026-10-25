# Iteration 469：全量离线迁移门禁与当前源码 DMG 验证

本轮没有新增后端业务迁移；完成当前源码的全量离线回归验证，并按用户明确要求重新构建 DMG。迁移目标持续有效，构建成功或测试通过不代表 Go 已退出。

验证通过：Rust workspace 全量测试与格式检查；`proletariusd` **1,474 passed / 0 failed / 3 ignored**；Go 全仓 `go vet ./...` 与 `go test ./...`；Swift BLE operation proof 和 BLE manager 回归；carrier 参数、USB 状态、ModuleRadio 投影、HTTP method/HEAD、IMS SDP/MESSAGE/USSD/号码归一化/错误诊断和 AMR framing 差分。所有测试均为离线测试，没有使用实体 SIM、读卡器或运营商网络。

Go 退出审计仍有 **6 类构建/运行阻塞**，包括 Xcode 编译/嵌入 Go、Rust 启停/转发 Go、Go module 和 **266 个生产 Go 源文件**；Rust-only route gate 仍有 **178 条**契约关联 Go。加权迁移估值维持 **37.99%（18.045/47.5，误差约±3个百分点）**，无实机验收，不增加迁移权重。

DMG：[`Switch-3.15-macOS27-iteration469.dmg`](../dist/Switch-3.15-macOS27-iteration469.dmg)，3.15/build315，52,272,002 bytes，SHA-256 `f166dfcbc7b75f6bcff5b2939133ff387b642812929543c9653ab2ae5aa23fc8`。`hdiutil verify` 通过；从映像挂载后验证 App 签名和版本。使用本机 ad-hoc 签名，没有 Apple 公证。

完整日志：[迁移门禁](../build/reports/iteration-469-migration-gates.log)。
