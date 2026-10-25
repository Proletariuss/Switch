# Iteration 389：REGISTER 临时失败退避策略迁入 Rust

生产读卡器 IMS 会话把 REGISTER 状态码与 carrier `temporary_status_codes` / `temporary_retry_seconds` 交给 Rust IMS codec，由 Rust 返回 registrar 重试 delay。默认临时状态仍是 408、500、502、503、504、480；显式空列表仍禁用模板状态；其他情况仍使用 150ms。Rust RPC 缺失或返回非法值时生产路径失败关闭。Go 继续负责等待、候选切换、REGISTER 重建、socket 与 session 生命周期；独立兼容构造未配置 Rust callback 时保留旧 Go fallback。

验证通过：Rust policy 单测 **3/3**、daemon codec 单测 **1/1**、Go imscore 与 macOS backend 测试通过；完整 `scripts/test-migration-gates.sh` 通过，包括 Rust workspace、Go 全仓 test/vet、Swift BLE/manager 回归和协议差分。迁移契约 `--diff` 通过：343 个 method/path（Rust 283 / Go 60）、127 个 Swift 调用点、359 个审阅范围。日志：[iteration-389-migration-gates.log](../build/optimization/iteration-389-migration-gates.log)，SHA256 `7021b10980313b870c3e315a0af6c7a5f4c10773631a7ae18f175fbab6eceddd`。没有连接读卡器/SIM、没有进行真实 IMS/短信/呼叫、没有生成 App/DMG。

P3 +0.025 计划人周等值，累计 **2.440 / 8.0 = 30.50%**；整体迁移累计 **16.920 / 47.5 = 35.6211%**（显示 **35.6%**，两位小数 **35.62%**，工程估算误差约 ±3 个百分点）。这只迁移重试策略计算，不代表 IMS 网络事务、恢复生命周期或 VoWiFi 闭环已迁出 Go。
