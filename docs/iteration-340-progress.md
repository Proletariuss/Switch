# Iteration 340：SIM 身份事务排队归 Rust USB AT owner

2026-10-10。源码和验证产物仅位于 `/Users/proletarius/Desktop/Switch`。本轮只使用伪 USB AT 传输；没有操作实体 SIM、联系运营商、拨号/发短信，也没有生成 DMG。

## 变更

- Rust USB AT owner 为完整 SIM 身份事务增加有界等待准入。新身份读取、AKA 和 Profile 检查会等已有 USB AT lease/操作退出后再竞争 owner；卡片检查仍在同一个不可插队事务内执行。
- 模块 VoWiFi Profile 身份复核移除了 Go `moduleSIMMu` 锁。卡命令及其等待现在由 Rust owner 排序，身份代次仍在实际读卡前复核；Go 继续保留每个 SIM session 的局部互斥和其他 USB AT 工作流的兼容锁。
- 其余 Rust owner 路径维持原来的立即冲突策略；等待队列仅用于 SIM 身份事务，不扩大 eSIM 写入或普通 AT 命令的等待行为。
- 无新增 HTTP/UDS 路由；迁移清单仍为 343 method/path（Rust 282 / Go 61）、127 个 Swift 调用点和 354 个审阅范围。

## 验证

- 新增 owner 并发测试：持有 SIM lease 时 Profile 复核等待且不发卡命令；释放后依序执行 QCCID、CIMI 并返回身份匹配。
- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,871 passed / 6 ignored**，daemon **1,333 passed / 3 ignored**；Go 全包/vet、Swift BLE、carrier、USB/radio/HTTP/codec/SDP/IMS 差分通过。
- Go VoWiFi SIM 适配定向测试、Rust 格式检查、契约清单差异检查和扫描器 43 项测试通过。
- 完整日志：[Iteration 340 migration gates](../build/optimization/iteration-340-migration-gates.log)。

## 进度与边界

P2B 保守增加 **+0.025 计划人周等值**，累计 **15.770 / 47.5 = 33.2%**（估值误差约 ±3 个百分点）。本轮只移除 Profile 身份检查路径的 Go 局部排队依赖；Go `moduleSIMMu` 仍保护其他跨工作流/兼容事务，模块 reader 物理生命周期、EAP/IMS、N待生产会话、实机验收及零 Go 构建/运行依赖仍未完成。9eSIM `6982` 不在本轮范围。
