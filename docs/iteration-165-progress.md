# Iteration 165：短信转发生产者触发链路端到端覆盖

日期：2026-10-08。测试均在 Switch 隔离运行目录内，使用本机 loopback Bark 模拟端点和合成短信；没有连接读卡器/SIM、访问运营商或外部网络、构建 App 或生成 DMG。

补上短信事件进入转发队列的跨进程集成证据。测试先记录 N 待变化游标高水位并提交已确认的 forwarding handoff，再经现有 Go IMS coordinator 产生 N 待入站事件，验证 Rust 持久 store 的新事件自动触发 Rust forwarding producer 并抵达本机 Bark。另经 Go 兼容端的 `archiveSMSChecked` 生产路径将合成设备短信写入 Rust durable archive，验证设备短信 Rust producer 独立读档并投递。两个事件正文各自只收到一次，继续跨过一个 producer 轮询周期后没有重复投递。

为了覆盖真实兼容调用关系，新增的 Go 路由只存在于集成测试 fixture：它调用生产 `archiveSMSChecked`，通过 Rust 内部认证 Unix socket 写入归档，不增加生产 API、不写 Go 第二份归档，也不连接任何硬件。

验证通过：`python3 scripts/test-multistandby-integration.py` **21项 Rust/Go 跨进程检查通过**，包括 Iteration 163 的进程强杀/恢复与未知投递不自动重放，以及本轮 N 待和设备短信 producer 到 loopback Bark 的自动投递。脚本重新编译 Rust daemon 与 Go fixture。Rustfmt、`git diff --check`、Python语法检查及迁移契约`--diff`均通过；契约保持291条（Rust224/Go67，含37个HEAD alias）、127个Swift调用点、272个审阅范围。Iteration 164 是最近一次完整 migration-gates 证据，本轮未重跑 Go 全仓、carrier replay、USB/radio/codec parity门禁。

保守计入 P2A **+0.025计划人周等值**，由3.860升至**3.885/5.5（70.64%）**；总体由13.720升至**13.745/47.5≈28.9%**（估算误差约±3个百分点）。外网 Telegram/Bark、系统 PAC、USB/SIM实体短信源、运营商 IMS、全格式/损坏存储矩阵与 Go-free 构建/运行仍未验收。
