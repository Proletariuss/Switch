# Iteration 426 — Rust-owned incoming INVITE response policy

日期：2026-10-12。

## 改动

Rust `IncomingCallTracker` 现在为生产 incoming INVITE admission/retransmission 决定 SIP response status/reason：初始 INVITE 与仍处 Trying 阶段的重传返回 `100 Trying`；Ringing 阶段重传返回 `180 Ringing`；Connecting/Active 重传返回 `200 OK`；新来电忙线或 tracker 判定 Busy 返回 `486 Busy Here`。

Go authenticated bridge 严格校验 action、phase、status 和 reason phrase 的允许组合，并拒绝多余 INVITE response metadata。IMS runtime 只消费 Rust 返回值并经现有 responder 写回。SIP parser/wire construction/socket、缺失/畸形 CSeq处理、SDP拒绝、媒体分配、dialog 与 session cleanup 仍由 Go 持有。本轮没有真实 SIM 或运营商呼叫测试。

## 验证

- `cargo test --manifest-path rust/Cargo.toml -p proletariusd incoming_invite`：2 passed。
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`：通过。
- `go test ./internal/vowifi/imscore ./cmd/proletarius-macos`：通过。
- `go test -race ./internal/vowifi/imscore ./cmd/proletarius-macos`：通过。
- `go vet ./internal/vowifi/imscore ./cmd/proletarius-macos`：通过。
- `python3 scripts/tests/test_rust_migration_contracts.py`：44 passed；migration census diff 通过，346 method/path（Rust286/Go60）、127 Swift 调用点、370 审阅范围不变。
- `scripts/test-migration-gates.sh`：通过。Rust workspace 1,425 passed / 3 ignored；Go 全包 test/vet、Swift BLE regression、carrier/USB/radio/HTTP/codec/IMS parity 均通过。
- 门禁日志：[iteration426-migration-gates.log](../build/optimization/iteration426-migration-gates.log)，SHA-256 `c2fc4ad4ae2bfe2d4058819a4872a96801d48e964a09f97bc72f014315af5bf2`。
- `git diff --check`：通过。

## 进度与边界

P5 增加 0.025 计划人周等值至 0.425/8.0；全项目 17.845/47.5，即 37.57%（估算误差约 ±3 个百分点）。Rust-only 退出审计仍有7类 Go 构建/运行/源码依赖及264个生产 Go 源文件。无运营商实机呼叫验收。本轮未生成 DMG；最新 DMG 仍冻结于 Iteration 424。
