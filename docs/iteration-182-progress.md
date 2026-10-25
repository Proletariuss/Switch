# Iteration 182：模块通话 CLCC 状态投影迁入 Rust

日期：2026-10-09。源码、运行时和验证产物均保留在 `/Users/proletarius/Desktop/Switch/`。

将旧 Go `AT+CLCC` 解析与通话状态归一化移至 Rust modem actor 的私有投影端点。该端点要求内部令牌，只接受严格 DTO；请求限制为 68 KiB、CLCC 原文限制为 64 KiB，不执行任何串口或卡片操作。Rust按旧规则处理来电、等待、拨号、振铃、活动及无号码的固件残留。Go仍持有串口查询、模块 SIM 事务锁、URC 回退、代次校验和通话记录触发；投影请求失败时只使用现有且未过期的来电 URC 缓存，否则返回 unknown。公开 `/api/call/status` 和通话控制仍由 Go 持有。

验证通过：Rust daemon **1,021 passed / 3 ignored**；Go 全仓测试与 `vet`、2,722 项载波回放、USB/radio/HTTP method+HEAD/codec+SDP parity通过。新增Rust CLCC投影用例3项、Go认证RPC/响应边界测试2项通过。迁移契约清单一致：313条 method/path（Rust247、Go66，43个HEAD alias）、127个Swift HTTP调用点、290个审阅范围；契约工具43项自测通过，`--diff`通过。`git diff --check`通过。

本轮未连接读卡器、号卡或执行AT/运营商操作；未进行Go-free构建/运行、实机验收、App打包或DMG构建。此切片将通话状态解析职责迁入Rust，但不计作公开通话API、IMS会话或模块硬件生命周期迁移完成。

保守计入P3 **+0.025计划人周等值**：从1.590提高到**1.615/8.0（20.2%）**；累计由14.270增至**14.295/47.5（30.1%）**，估算误差约±3个百分点。P2A为3.960/5.5（72.0%），P2B为5.100/8.0（63.8%），P4为0.750/4.5（16.7%），eSIM/LPA为1.125/5.5（20.45%）。完整 IMS/SIP生命周期、通话控制/媒体、VoWiFi、reader/profile租约、N待多Profile及Go-free退出验收仍未完成。
