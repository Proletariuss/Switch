# Iteration 178：监督模式下设备信息 AT 查询序列迁入 Rust

日期：2026-10-09。所有源码和验证产物均位于 `/Users/proletarius/Desktop/Switch/`。

监督模式中的模块设备信息采集现在通过认证的 Rust modem actor 一次执行既有 AT 查询顺序，并返回固定的设备信息 DTO。Rust 保持旧字段解析和逐项 best-effort 语义；SIM 插入状态保留原有 CPIN 回退，蜂窝信息受抑制时跳过对应探测。Go 继续负责把快照投影到现有设备缓存。显式独立兼容采集路径仍保留旧查询实现，因此尚未移除 Go 硬件适配或完成模块设备 owner 迁移。

验证结果：

- Rust modem actor 的设备信息字段/命令顺序与蜂窝信息抑制测试通过；Go 快照投影、兼容语义和认证 RPC DTO 测试通过。
- `scripts/test-migration-gates.sh` 全部通过：Rust workspace、Go 全仓测试与 `vet`、2,722 项载波回放、USB/radio/HTTP method+HEAD/codec+SDP parity 均通过。
- 单独复核 `proletariusd` 测试：**1,011 passed、0 failed、3 ignored**。迁移契约为 311 条 method/path（Rust 245、Go 66，含 42 个 HEAD alias）、127 个 Swift HTTP 调用点和 287 个审阅范围。
- 格式检查与 `git diff --check` 通过。测试未连接读卡器、号卡或运营商网络；未进行实机及 Go-free 构建/运行验收，未打包 App 或 DMG。

保守计入 P2B **+0.025 计划人周等值**，由 5.000 增至 **5.025/8.0（62.8%）**；总体由 14.120 增至 **14.145/47.5≈29.8%**（估算误差约 ±3 个百分点）。全 Rust 后端目标仍未完成。
