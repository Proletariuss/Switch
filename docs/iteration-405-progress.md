# Iteration 405：USSD 初始 INVITE 响应策略迁入 Rust

2026-10-12

读卡器 IMS 生产 USSD 流程现在通过既有认证 IMS codec endpoint，将 INVITE 临时/最终响应状态、dialog 是否已建立和 To tag 是否存在提交给 Rust。Rust 返回明确的 transition：忽略临时响应、拒绝、建立 dialog、确认已建立 dialog 的重传，或报告缺失远端 tag。Go 校验 action 并负责发送 ACK、解析 Contact/Record-Route、更新 dialog、组织队列和清理会话；没有把 SIP I/O 或状态副作用移入 Rust之外的层，也没有新增 method/path。未接触真实卡或运营商。

Rust policy/daemon operation、Go fallback、生产 callback 注入和 authenticated Unix-socket bridge 定向测试通过；USSD Go 测试 race 检查通过。完整 `scripts/test-migration-gates.sh` 通过：Rust daemon 1,413 passed / 3 ignored，Go 全包 tests/vet、Swift BLE 回归及 carrier/USB/radio/HTTP/codec/SDP/AMR/MESSAGE parity 通过。接口仍为 346 method/path（Rust 286 / Go 60）、127 Swift HTTP 调用点、362 reviewed scopes；`api-contract-review.json` 更新后 `--diff` 通过，无新增路由。日志 [`iteration-405-migration-gates.log`](../../build/optimization/iteration-405-migration-gates.log)，SHA-256 `deefe290ce316073411ea99d242527291335fb65a13e7266d084b1bff464a0a9`。

P3 +0.025，累计 **2.615 / 8.0**；全项目 **17.320 / 47.5 = 36.46%**（显示 36.5%，估算误差约 ±3 个百分点）。3.06/build306 DMG止于Iteration404，不含本轮源码。尚未完成全Rust改造或真实 IMS/USSD 验收。
