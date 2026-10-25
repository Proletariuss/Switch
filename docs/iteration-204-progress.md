# Iteration 204：IMS 组合网络 owner 接入 daemon 会话边界

日期：2026-10-09

## 实现

- `NativeTunnelOwner::start_ims_network` 从该实例的已认证 `NativeTunnel` 建立 `ImsNetworkRuntime`，将动态 IMS TCP socket 控制和 IPsec security 控制放进同一 tunnel network task；owner 持有可 await 的 JoinHandle。
- 返回的 `NativeImsNetworkControl` 绑定隧道配置、TCP/IPsec controls、IKE stop 和 reader lease-loss 信号；REGISTER 适配器只能从该对象取配置和 `RegistrationServices`，不再接受独立 tunnel 与可任意拼装的 controls。
- network future 同时持有 tunnel 和 reader lease heartbeat；现有 `RegisteredImsSession::shutdown` 先完成 protected IMS 清理，再通知 IKE 停止。

这条路径仍处于 gated native implementation：没有被 keyed lifecycle actor 或生产会话路由调用。因此没有宣称 Go IMS owner 已退出，不增加加权进度。下一步需要把配置准备、session generation、tunnel startup、REGISTER、SIP/SMS/call event dispatch、lease loss 及分阶段 shutdown 纳入同一个 actor，并继续维持 BLE/USB reader authority 与 Go 兼容期的单 owner 约束。

## 验证

- `cargo +1.98 test --offline --manifest-path rust/Cargo.toml -p proletariusd native_vowifi_session::tests`：13 passed。
- `cargo +1.98 fmt --manifest-path rust/Cargo.toml --all -- --check`：通过。
- `git diff --check`：通过。
- `scripts/test-migration-gates.sh`：通过。Rust workspace 的 `proletariusd` 为 1,106 passed / 3 ignored；Go 全仓 `go vet`/`go test`、2,722 项载波回放、USB/radio/HTTP method+HEAD/PCMA/SDP/AMR parity通过。
- `python3 scripts/check-rust-migration-contracts.py --require-rust-only`：未通过，默认 Go fallback 存在、177 条路由仍有 Go owner/dependency、7 个变更文件待人工复核且契约清单漂移；未刷新清单。`python3 scripts/check-rust-backend-exit.py`：阻断，列出 7 类 Go 构建/运行/源码残留。

整体进度仍为 **14.795/47.5（31.1%）**。没有连接实体读卡器/SIM、访问运营商、发送短信或拨号，也没有构建 App/DMG；9eSIM `6982`仍排除。
