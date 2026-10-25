# Iteration 332：PC/SC USIM/ISIM 元数据读取移交 Rust

2026-10-10。所有源码和验证材料均在 `/Users/proletarius/Desktop/Switch`。本轮未连接或操作实体 SIM、未发短信/拨号、未生成 DMG。

## 变更

- Rust 读卡器身份端点现在在同一授权 PC/SC 事务中读取并返回完整快照：ICCID、IMSI、MCC/MNC、USIM AID、GID1/GID2、SPN、MSISDN、PNN 名称回退、SMSC/PSI，以及可用时的 ISIM AID、IMPI、IMPU 和 home domain。
- PC/SC 刷新收到 Rust 快照后直接构造应用 `usimInfo`，不再调用 Go `usim.New`，也不再由 Go 发出身份/元数据 APDU。Rust 不可用时仍保留既有兼容路径；BLE/模块身份初始化仍在 Go。
- 可选 EF 缺失或格式错误继续按可选字段处理；底层 APDU、FCP 解析和文件长度均受边界约束。端点等待限时提高到 45 秒，以覆盖完整 metadata 序列。

## 验证

- `cargo test -p proletarius-reader`：30 passed；`cargo test -p proletariusd --bin switchd-rs reader_identity`：9 passed。
- Go `cmd/proletarius-macos` 定向测试通过。
- 最终 `scripts/test-migration-gates.sh` 退出码 0，日志：[Iteration 332 migration gates](../build/optimization/iteration-332-migration-gates.log)。包括 Rust workspace 测试/格式、Go 全包测试与 vet、Swift BLE 回归和现有跨语言协议差分。
- 仍未进行实体卡或运营商实测；此项只证明 PC/SC 代码路径和离线兼容门禁，不证明 BLE 元数据、9eSIM 下载或 VoWiFi 线上业务已完成。

## 迁移进度

按 P2B 计入 **+0.050 计划人周等值**：完整 metadata APDU 生产路径在 PC/SC 模式退出 Go `usim.Card` 初始化，Rust 快照直接用于缓存对象构造。保守不计 BLE/模块路径、Go 兼容 fallback 和硬件实测。累计 **15.470 / 47.5 ≈ 32.6%**（估算误差约 ±3 个百分点）。

下一步迁移 BLE 身份 metadata 的 APDU 生产通路，并统一 PC/SC/BLE 的身份快照与代次发布流程；之后再移除 Go 兼容初始化，完成 reader 生命周期和最终零 Go 运行验收。
