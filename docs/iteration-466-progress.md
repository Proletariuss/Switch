# Iteration 466：语音租约获取在请求取消时完成清理

对 Iteration 465 的 Rust-owned Voice lease 增加取消安全性。旧 USB AT 句柄关闭可能因真实物理 I/O 暂时阻塞；如果调用者在 `acquire_voice_transition` 返回前断开，之前的获取 future 会被取消，而 Voice lease 已写入 owner 状态，存在遗留租约风险。现在获取在 owner task 中继续执行，并通过 one-shot 交付 lease；若接收方已取消，owner task 会立即显式释放已获取租约并检查清理结果。正常未取消路径仍由调用方持有 lease，helper start/stop 完成后确认释放。

新增确定性测试在 fake transport 的 close 阶段阻塞期间取消等待任务，随后放行物理关闭，并验证 owner 清掉租约、结束 busy/revoking 状态且关闭恰好一次。此测试覆盖取消清理语义，不模拟真实 USB 硬件。

验证：`cargo +1.98.0 test --locked -p proletariusd` 全 crate **1,468 passed / 0 failed / 3 ignored**；定向取消用例通过；`cargo +1.98.0 fmt --all -- --check` 通过；合同扫描器 **45 passed**；合同 `--diff` 通过：354 method/path（Rust293、Go61，含51 HEAD alias）、127 Swift HTTP 调用、379 review scopes；`git diff --check` 通过。Go/Swift/设备流程未在本轮改变。

该改动加固 USB owner 的取消边界，未迁移新的完整业务能力，不满足任何新的 P2B/语音端到端计分点。总体仍为 **37.99%（18.045/47.5，约±3个百分点）**。Go 通话控制、SIP/RTP/音频生产生命周期和硬件验收仍未完成；本轮没有访问真实设备或生成 DMG。

详见[Iteration 465](iteration-465-progress.md)了解该 lease 的首次 owner 迁移；全量测试日志见[`iteration-466-proletariusd-tests.log`](../build/reports/iteration-466-proletariusd-tests.log)，合同自测和差异见[`iteration-466-contract-selftest.log`](../build/reports/iteration-466-contract-selftest.log)与[`iteration-466-contract-check.log`](../build/reports/iteration-466-contract-check.log)。
