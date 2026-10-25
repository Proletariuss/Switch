# Iteration 167：N待操作执行权改由 Rust 耐久领取

日期：2026-10-08。所有改动和测试产物均留在 Switch。本轮未连接读卡器/SIM、访问运营商或真实 provider，未构建 App，也未生成 DMG。

Go 不再保存进程内的 N待 operation ID 去重表。经内部凭证验证并校验卡片/Profile 后，Go 执行器先请求 Rust 操作账本领取执行权；Rust 在单次原子写入中将操作从 `pending` 转为 `running`。并发或重复回调只有一个能够领取，其余收到已处理响应而不启动 worker。Rust 重启仍会把 `running` 隔离为 `unknown`，不会再次放行。停止期间仅允许已领取操作的状态回调完成，新领取会被拒绝。公开 start/stop/retry 的响应、路由方法与状态字段不变。

最终安全复核还将 claim 请求绑定到 Rust 已受理记录中的 action、card ID 和 Profile ID 集合。执行回调若拿同一个 operation ID 携带不同操作参数，Rust 返回冲突且不消耗 pending claim；Go 将完整命令交给 Rust 校验，而不是只凭 operation ID 领取。

Go 仍负责实际读卡、Profile 会话和物理操作，并将最终状态回调 Rust。本轮迁移的是操作执行权与重复保护的耐久 owner，不代表 N待 broker、IMS/VoWiFi 生命周期或设备会话已离开 Go。

验证通过：定向 Rust 操作账本测试 **8项**，其中8个并发领取者只有一个赢家，并验证重启后不可重新领取；Go 执行器定向测试通过；Rust supervisor + Go fixture 跨进程集成 **22项**全部通过。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,456通过、6忽略**（其中 daemon **959通过、3忽略**）；Go 全仓 `vet`/测试、载波回放 **2,722项**、USB/radio/HTTP-HEAD、语音编码/SDP差分通过；Rustfmt、迁移契约 `--diff` 通过。最新契约清单 **295条**（Rust **231** / Go **64**，含 **38** 个 HEAD alias）、127个 Swift HTTP调用点、277个审阅范围。Rust-only门禁仍有默认Go fallback和**149条** Go owner/dependency路由，故零Go构建/运行尚未实现。

保守计入 P4 **+0.025计划人周等值**，由0.500增至 **0.525/4.5（11.67%）**；总体由13.770增至 **13.795/47.5≈29.0%**（估算误差约±3个百分点）。实卡、多Profile并行、broker、Go-free build/runtime和长稳验收仍未完成。
