# Iteration 079：将固定 modem 查询移交 Rust actor

日期：2026-10-05。所有源码与构建文件仍只在 `/Users/proletarius/Desktop/Switch/src` 中修改；Celmux 参考目录未改动。

## 实现

新增认证私有 `POST /api/internal/modem/typed-query`。请求只接受固定查询名，不允许传入 AT 文本；白名单为 IMEI、固件版本、SIM 在位、IMSI、ICCID 和 CSQ。Rust modem actor 将查询命令放入同一有界 AT 调度器，并负责结果解析。SIM 在位查询保留旧逻辑：先尝试 `AT+QSIMSTAT?`，无法识别时再发 `AT+CPIN?`。

Go `Manager` 的既有查询方法在生产 Rust 串口端口上调用该能力，继续返回原来的 Go 类型；CSQ 仍先执行飞行模式蜂窝探测抑制。未实现 typed-query 的兼容串口端口仍保留原解析路径，因此测试和旧环境保持兼容。公开 Swift API、设备身份结构、采集顺序和持久化格式没有变化。新内部端点沿用既有 UDS token、请求体预算和 shutdown admission。

## 验证

- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletariusd`：**351 项通过**。包含伪串口验证 Rust 实际发出固定 `AT+CSQ` 并解析 `+CSQ: 17,99`；亦覆盖解析兼容与拒绝未列入白名单的查询。
- `GOPROXY=off GOSUMDB=off GOTOOLCHAIN=local go test ./internal/modem -count=1`：通过，含 Manager 保留空身份值、CSQ 飞行模式抑制与查询名一致性检查。
- `go test ./cmd/proletarius-macos -run TestRustModemPortUsesAuthenticatedInternalSocket -count=1`：通过，验证私有 socket JSON 字段、Rust 返回值解码和任意 AT 请求拒绝。
- `python3 scripts/check-rust-migration-contracts.py`：通过，196 项 method/path 契约（Rust 93、Go 103）、127 个 Swift 调用点及 69 项审阅指纹一致。
- Rustfmt、Go fmt 与 `git diff --check`：通过。保留两个与本轮无关的 `native_vowifi_session.rs` dead-code warning。

没有运行生产 App、访问真实读卡器/蜂窝模块、发送短信或生成 DMG。端到端实体设备差分仍是 P2B 验收项；Go manager、URC业务分派、其余硬件查询和兼容回退仍存在，不代表 modem backend 已全 Rust 化。

## 进度

该读取路径已接入 Rust 生产 owner 并通过离线兼容检查，按工期权重将 P2B 硬件/LPA 接管从 28% 调整为 30%（8.0 人周工作包增加 0.16 人周等值）；总体估算从 **21.1%（10.0/47.5）** 调整为 **21.4%（10.16/47.5）**，误差约 ±3 个百分点。该比例不代表可发布或实体设备验收完成。当前契约入口统计变化为 Rust 93/196；入口数不等于端到端迁移率。
