# Iteration 082：Rust 接管网络制式 COPS 回退查询

日期：2026-10-05。源码和生成记录仅在 `/Users/proletarius/Desktop/Switch/src` 内变更；未修改 Celmux 参考目录。

## 实现

`Manager.QueryNetworkModeFallbackAndDuplex` 在 `AT+QNWINFO` 返回空制式或失败时，原先由 Go 再发 `AT+COPS?`，从最后一个字段读取 access technology。现在新增固定 typed-query 名称 `network_mode_fallback`，Rust modem actor 只发送 `AT+COPS?` 并将 AcT 0/2/7/10 映射为 `GSM/WCDMA/LTE/NR`。它不接收调用方 AT 文本，也不修改网络选择。

Go 保留主查询成功且非空时不回退、蜂窝探测抑制、回退失败时优先返回主查询错误、fallback 双工字段为空，以及没有 Rust typed-query owner 时的旧串口兼容路径。typed-query 仍是原有认证内部端点，本次没有新增 method/path。

## 验证

- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletariusd typed_query`：3 项匹配测试通过，解析覆盖 GSM/WCDMA/LTE/NR 与 malformed 值；伪串口覆盖空 QNWINFO 后的固定 COPS 命令顺序。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./internal/modem -count=1`：通过，覆盖主结果为空、fallback 结果、查询顺序及原查询错误优先级。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./cmd/proletarius-macos -count=1`：通过，Unix-socket fixture 覆盖新固定查询名称和 JSON projection。
- 同一 Rust typed-query 命令、`cargo fmt --check`、Go `gofmt -d`、`git diff --check` 与迁移契约扫描均通过；method/path census 保持 196 项。

没有连接实体模块或进行真实无线查询，没有发送短信、改写卡片或生成 DMG。

## 进度

该固定回退查询计入 P2B modem-query owner 的小幅增量：P2B 从 33.4% 调整为 **33.5%**，加权工作量从 10.43 调整为 **10.44 / 47.5 人周**；整体仍为 **22.0% 已完成 / 78.0% 待完成**，估算误差约 ±3 个百分点。它不代表模块完整 owner 接管或实体设备验收。
