# Iteration 176：设备状态投影与 AT 串口发现迁入 Rust

日期：2026-10-09。所有改动与验证产物均位于 `/Users/proletarius/Desktop/Switch/`。

本轮完成两项独立后端切片。

Rust 网关现在生成兼容的公开 `GET /api/status` 响应，`device-snapshot` 复用同一投影。监督模式下，Rust 通过认证私有 `POST /api/internal/runtime/status-snapshot` 向 Go 事实适配器取 USB/eSIM 观测；读取前后验证 Go 子进程代次稳定，并严格校验必需 DTO 字段，同时保留未知 JSON 字段以兼容未来扩展。`HEAD /api/status` 与 GET 复用路由策略和状态，但不返回响应体。显式 Go 兼容模式继续使用旧处理器。此次迁移改变的是公开投影 owner，实际硬件观测仍在 Go。

模块启动时的串口候选发现也迁入 Rust modem actor。认证私有 `POST /api/internal/modem/discover-at-port` 沿用原来的 `cu.usbmodem*`、`cu.usbserial*`、`cu.wchusbserial*` 候选模式、优先级和排序，并限制最多32个候选。发现动作遇到活动串口、AT命令、队列、短信接收worker或初始化时会拒绝执行，避免与实际I/O争抢。每个候选由 Rust actor 的端口打开器以有界 `AT` 和 `AT+CGSN` 探测确认后立即释放；响应长度、单端口探测和总请求都有上限。Rust仅返回 `/dev/cu.*` 设备路径。正常由 Rust 监督的运行路径中，Go 不再枚举、打开或探测串口；显式独立 Go 兼容启动仍保留旧发现回退，后续 modem bootstrap 和其他设备业务仍在 Go。

验证结果：

- `scripts/test-migration-gates.sh` 全量门禁通过：Rust daemon **991 passed/3 ignored**；Go 全仓 `vet`/测试、2,722 项 carrier replay、USB/radio/HTTP method+HEAD/codec+SDP parity通过。
- `scripts/test-multistandby-integration.py` 的 **30 项**监督进程跨语言集成通过。
- 迁移契约扫描器 **43 项**自测通过；契约 `--diff` 通过。最新清单307条method/path（Rust240、Go67，含40个HEAD别名）、127个Swift HTTP调用点、286个审阅范围。
- 状态接口覆盖 GET/HEAD 兼容、内部认证、代次变化拒绝、未知遗留字段保留和 HEAD bodyless；AT发现覆盖候选排序、伪串口探测、actor忙碌拒绝及不安全路径拒绝。

进度按已实现且通过离线验证的工作包保守记账：公开状态投影计入 P2A **+0.025计划人周等值**，由3.910升至**3.935/5.5（71.5%）**；AT串口发现计入 P2B **+0.025**，由4.950升至**4.975/8.0（62.19%）**。总进度由14.045升至**14.095/47.5≈29.7%**（估算误差约±3个百分点）。契约路由数量只用于接口清点，不代表整体迁移完成率。

`--require-rust-only` 仍因 Go fallback 和业务依赖受阻。尚未完成 Go-free 构建/运行、设备与运营商验收；本轮未操作实体读卡器、号卡或运营商网络，未运行生产 App，未构建 DMG。
