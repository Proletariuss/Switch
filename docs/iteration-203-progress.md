# Iteration 203：统一 IMS 隧道网络运行时边界

日期：2026-10-09

## 变更

在 `rust/crates/proletarius-vowifi/src/ims_network_runtime.rs` 新增 `ImsNetworkRuntime`，将 `TcpSocketRegistry` 与 `SecurityNetworkRuntime` 组合为一个 `NetworkRuntime`。构造函数返回 `TcpControl`、`SecurityControl` 以及唯一运行时实例，两类控制句柄共享一个 wake sender。后续 native session actor 可将控制句柄交给 `RegistrationServices`，并将该 runtime 交给 `NativeTunnel::run_network_with_runtime`，让 SIP socket 生命周期和 IPsec ESP transform 由同一个 tunnel network task 服务。

## 验证

- `cargo +1.98 test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi ims_network_runtime::tests::composed_runtime_routes_ims_tcp_and_stops_both_control_planes`：1 passed。
- `cargo +1.98 test --offline --manifest-path rust/Cargo.toml -p proletarius-vowifi`：148 单测 passed、2 个 carrier integration passed、doc tests passed。
- `cargo +1.98 fmt --manifest-path rust/Cargo.toml --all -- --check`：通过。
- `git diff --check`：通过。

隔离回归在双端 `TunnelNetwork` 间完成 SIP REGISTER/401 字节往返；客户端通过共享 `SecurityControl` 执行 install/clear，再停止两端运行时。测试使用合成地址、密钥和服务端，不连接物理读卡器、真实卡片或运营商。

## 接管边界与进度

Iteration 204 随后在 daemon 会话边界加入受同一 `NativeTunnelOwner` 约束的控制句柄，REGISTER 适配器不再接收可被误配到其他 tunnel 的独立 TCP/IPsec controls。它仍未被 keyed actor 或公开生产会话路由调用，所以本次边界改进**不增加加权完成值**；真实 tunnel、reader lease、IMS AKA/REGISTER、消息/来电分发和关停仍未完成 Go owner 退出。Iteration 203 的完整离线迁移门禁随后通过：Rust workspace、Go 全仓 `go vet`/`go test`、2,722 项载波回放及 USB/radio/HTTP method+HEAD/PCMA/SDP/AMR parity 均通过。整体仍为14.795/47.5计划人周（31.1%）；9eSIM `6982`继续排除。未接触真实设备、未构建 App 或 DMG。

## 下一步

将该组合 runtime 接入 per-reader keyed native session actor，并实现/验证 lease-bound IKE 启停、REGISTER deadline/AKA、registered-session事件分派，以及 IMS 先于 IKE 的确定关停。只有 Rust actor 实际持有并驱动生产 tunnel/IMS资源后，才计入生产接管完成度。
