# Iteration 333：BLE 身份 metadata APDU 序列移交 Rust

2026-10-10。所有源码与验证材料均在 `/Users/proletarius/Desktop/Switch`。未连接或操作实体 SIM、未发送短信/拨号、未生成 DMG。

## 变更

- 将 BLE metadata 从 Go 逐条调用 APDU 改为单次 Rust `metadata` 操作。Rust worker 持有原 reader generation、producer 和 transaction capability，在唯一 BLE I/O permit 内执行 USIM/ISIM metadata 读取，并复用 Iteration 332 的 Rust 解析与快照格式。
- Go BLE adapter 仅提交 metadata 操作并接收 Rust snapshot；在 BLE Rust transport 未配置时明确失败关闭，不再退回 Go `usim.New` 逐条读取。
- 保留 Swift CoreBluetooth 桥职责。Rust 对 metadata 执行期间的桥接失败仍提取受限错误码，不把 APDU 包装层错误退化成通用错误；对外错误不返回卡片字段或自由文本。

## 验证

- 新增 Rust 测试覆盖 metadata command 的 reader/capability 绑定、拒绝携带调用方 APDU，以及 BLE 卡错误在 metadata worker 中失败关闭并保留错误码。
- 新增 Go 测试核对 metadata RPC 的 operation、reader、transaction 绑定和返回 snapshot 解码。
- 最终 `scripts/test-migration-gates.sh` 退出码 0；Rust workspace、Go 全包/vet、Swift BLE 回归及现有跨语言差分通过。日志：[Iteration 333 migration gates](../build/optimization/iteration-333-migration-gates.log)。
- 未实测 BLE 设备或 SIM。代码门禁不能证明 B1 真机握手或运营商业务已通过。

## 迁移进度

按 P2B 计入 **+0.050 计划人周等值**：BLE metadata 生产 APDU 序列由 Rust 在单一授权 worker 执行，Go 不再发出字段级 APDU 或构造 Go `usim.Card`。保守不计 Swift 系统桥、BLE LPA/AKA 的其他 Go 调用、缓存代次统一发布、读卡器完整生命周期和实机验收。累计 **15.520 / 47.5 ≈ 32.7%**（估算误差约 ±3 个百分点）。

下一步收敛 PC/SC/BLE metadata snapshot 的 generation prepare/read/commit 与缓存发布，拆除剩余 Go metadata fallback；再继续迁移 BLE LPA/AKA 和读卡器生命周期，最后完成零 Go 构建/运行验收。
