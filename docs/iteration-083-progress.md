# Iteration 083：Rust modem actor 接管 EF-SPN 查询

日期：2026-10-05。所有可编辑源码与进度记录均在 `/Users/proletarius/Desktop/Switch/src`；Celmux 仅作只读参考，未修改原始目录。

## 实现

Rust modem actor 新增固定只读 typed-query `native_spn`，在单一串口 owner 内执行既有命令 `AT+CRSM=176,28486,0,0,17`。它校验 CRSM 状态字及十六进制长度，并解码 EF-SPN 的 ASCII、GSM 7-bit、普通 UCS2、`0x81` 和 `0x82` 压缩 UCS2 格式。Go `Manager.QueryNativeSPN` 优先调用该查询，继续保留原错误上下文与无 Rust owner 时的兼容路径。私有 UDS method/path 没有增加；公开响应行为不变。

本项遵循 Celmux AT session 的“一个设备只有一个事务 owner”原则，但没有复制其 PolyForm AT 实现。其余 Celmux 可复用组件与尚未采用的候选，见[复用评估](optimization/2026-10-04-celmux-reuse-review.md)的 2026-10-05 复核。

## 验证

- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletariusd typed_query`：parser、伪串口命令顺序与拒绝路径测试通过。
- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletariusd`：351 项通过，0 失败。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./internal/modem -count=1`：通过。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./cmd/proletarius-macos -count=1`：通过，覆盖 UDS 名称、JSON projection 与错误上下文。
- `./scripts/check-rust-migration-contracts.py`：196 项 method/path census 与当前源码一致；此扫描不代表 Go-free 验收。
- Rust formatting、Go formatting diff 与 `git diff --check` 通过。

没有连接实体卡/模块，没有执行真实 CRSM I/O、发短信、改写 eSIM，也没有生成 DMG。daemon 现存的两个 `native_vowifi_session.rs` dead-code 警告及 macOS Go 链接的重复 `-lusb` 提示与本迭代无关。

## 进度

该项把一条固定只读 modem 查询的执行和格式解析迁入 Rust，并保留 Go 兼容边界；据此将 P2B 从 33.5% 调整为 **33.7%**，加权累计由 10.44 调整为 **10.451 / 47.5 人周**。整体进度按工期中值加权仍为 **22.0% 已完成 / 78.0% 待完成**（约 ±3 个百分点）。它不代表 AT 硬件 owner 完成，也不包含实体设备验收。
