# Iteration 081：Rust 接管 serving operator 查询

日期：2026-10-05。源码与记录仅在 `/Users/proletarius/Desktop/Switch/src` 内变更；`/Users/proletarius/Downloads/celmux-src` 只读。

## 实现

参考 Celmux AT modem 的 operator 查询行为，把 Go `Manager.QueryOperator` 的 Rust owner 路径接入现有认证私有 `typed-query` 白名单。Rust actor 对 `operator` 固定执行 `AT+COPS=3,2`（保持旧实现的 best-effort 语义），随后执行 `AT+COPS?` 并只从带引号的 serving PLMN 字段提取号码；第二条命令失败时返回错误。第一条命令超时则尝试发送 ESC，再继续查询。没有引入任意 AT 输入，也没有复制 Celmux 的 PolyForm AT 应用代码。

Go 继续处理原有 PLMN 到运营商显示名的映射，保留蜂窝状态抑制和未配置 Rust executor 时的串口兼容路径。该改动使用既有内部 method/path，只扩展固定查询名称，因此 HTTP/UDS route census 数量不变。

## 验证

- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletariusd typed_query`：3 项匹配测试通过，包含格式选择返回 `ERROR` 后仍执行 `COPS?`、PLMN 解析及 PTY actor 流程。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./internal/modem -count=1`：通过，覆盖 `46011` 到“中国电信”的旧显示映射及飞行模式抑制。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./cmd/proletarius-macos -count=1`：通过，Unix socket fixture 验证 Rust typed-query JSON 投影和固定名称白名单。
- API/UDS 契约扫描通过：196 个 method/path contract，Rust owner 93、Go owner 103、Swift 调用点 127、69 个 fingerprinted review scopes。扫描仅核对清单一致性，不代表路由均已端到端 Rust 化。
- `cargo +1.98 fmt --manifest-path rust/Cargo.toml --all -- --check`、`gofmt -d` 与 `git diff --check` 通过。

未连接读卡器或 modem，未执行实体运营商查询、短信或卡片操作；未生成 DMG。`AT+COPS=3,2` 只设置后续 `COPS?` 的数值格式，不切换运营商或无线网络。

## 进度

新增一项生产接线和离线兼容测试后，P2B 从 33.0% 调整为 33.4%；按 47.5 人周加权基线，全项目从 21.9%（10.40/47.5）调整为 **22.0%（10.43/47.5）**，估算误差约 ±3 个百分点。该进度不代表实体硬件验收或全 Rust 后端完成。
