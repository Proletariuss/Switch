# Iteration 171：N 待运行观察事实归一化迁入 Rust

日期：2026-10-08。源码、测试与构建材料均在 `/Users/proletarius/Desktop/Switch/`。

Go 运行时观察现在只发送时间、Profile ID、代次、短原因码、`desired` 和隧道、IMS 注册、消息服务、SIM 短信路由的就绪事实，不再先行判定报告状态或恢复标记。Rust 对原因码进行白名单归一化，并根据这些独立事实及与原行为一致的优先级推导 `stopped`、`not_ready`、`unknown`、`online` 和恢复标记，再持久写入既有 `multi-runtime-events.json` Event 格式。旧 journal 及公开报告结构保持不变；非法字段、原因码形状或 Profile ID 继续 fail closed。Go 仍采集真实 IMS/隧道和短信路由就绪事实，并持有会话本身和在线累计计时事实。

Rust 定向报告/恢复测试 **18 项**通过；全量门禁通过：Rust workspace **1,464 项通过、6 项忽略**（daemon **967 项通过、3 项忽略**），Go 全仓 `test` / `vet`、2,722 项载波回放及 USB/radio/codec/HTTP-HEAD parity 通过；监督进程跨语言集成 **26 项**通过。迁移契约 `--diff` 与 **43 项**工具自测通过；清单为 **296 条**（Rust 232、Go 64，含 38 个 HEAD alias）、127 个 Swift 调用点、277 个审阅范围。Rust-only 门禁仍因默认 Go fallback 和 150 条 Go owner/dependency 路由未通过。未操作实体卡、未构建 App 或 DMG。

保守计入 P4 **+0.025 计划人周等值**，至 **0.625/4.5（13.89%）**；总体 **13.895/47.5（29.3%）**，误差约 ±3 个百分点。Go N 待卡片仲裁、真实会话/broker、逐 Profile timer、硬件生命周期、Go-free 构建运行及设备长稳验收仍未完成。
