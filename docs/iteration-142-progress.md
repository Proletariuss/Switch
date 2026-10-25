# Iteration 142：网络服务与流量接口移除 Go health 依赖

日期：2026-10-08。全 Rust 后端目标持续实施；本轮所有源代码、构建和验证产物均位于 `/Users/proletarius/Desktop/Switch/`。

## 实施

Rust `GET /api/network/services` 和 `GET /api/network/traffic` 不再通过 Go `/api/health` 获取 USB 产品名。两条路由先从同一 Rust USB AT owner 的状态锁读取已打开句柄的设备描述；若当前没有活动 USB AT 句柄，则调用已经用于网络诊断的有界 `ioreg` DJI 设备发现。它不另开设备句柄、不发送 AT 命令，也不操作 SIM。

提取并单测了 `ioreg` 解析，保持 Go 的十进制属性解析、`2ca3` Vendor 过滤、默认 `DJI 4G Module` / `DJI` 名称、接口排序和 QMI/diagnostic mode 判定。服务列表仍按产品名匹配 `Hardware Port`；无设备时模块标记为 false，流量接口保留原来的“未识别到 4G 模块”响应。与旧 Go `/api/health` 不可用不同，Rust 路由现在仍能在 Go 服务退出时完成本地只读采样，这是消除此依赖的预期行为。

更新了 `handle_ready_request`、`network_diagnostic.rs` 和 `usb_at.rs` 的人工审阅指纹以及路线清单。277条 method/path、Rust199/Go78、31个派生HEAD alias、127个Swift调用点、225个审阅范围保持不变；Rust-only剩余Go所有权/依赖路由由147降至143。默认Go fallback仍在，因此零Go收口尚未达成。

## 验证

- `network_diagnostic`：4项通过；覆盖DJI解析、默认字段、非DJI过滤、诊断投影与部分失败。
- `usb_at` active-description lifecycle：1项通过；覆盖未打开、打开、关闭后的设备描述状态。
- `network_services`：6项通过；`network_traffic`：5项通过。
- `scripts/test-migration-gates.sh`：通过；Rust workspace 与既有 modem/USB、载波、射频、codec、HTTP/HEAD 兼容检查通过，`proletariusd` 836项通过、3项忽略；Go 全仓 `go test ./...` 和 `go vet ./...` 通过。HTTP/HEAD 冻结 Go mux 测试通过。
- 迁移工具自测42项通过，`check-rust-migration-contracts.py --diff` 通过。`--require-rust-only`按预期失败：默认 Go fallback 存在，143条路由仍有 Go owner/dependency。
- `git diff --check` 通过。Release `Switch.app` 2.93/build293 arm64 已构建，`codesign --verify --deep --strict` 通过。内嵌 `switchd-rs` SHA-256：`b7e6347054935fd2a814dd5b6509d74e0092f34ca88a49b3a4eb9f91aa86a4d5`。
- 本轮未连接/操作实体模块、读卡器或SIM，未执行AT、通话或短信，也未生成 DMG。此前接听/挂断布局修正包含在该 App 构建内，不计入后端迁移比例。

验证日志：`build/optimization/iteration-142/migration-gates.log`、`contract-selftest.log`、`contract-check.log`、`rust-only-gate.log` 和 `release-build.log`。

## 进度

这是一段只读网络身份依赖收敛，不等于网络配置、Go fallback 或模块后台整体完成。P2A计入 **+0.025计划人周**，从3.735提高到**3.760/5.5（68.36%）**；全项目从13.095提高到**13.120/47.5≈27.6%**（估算误差约±3个百分点）。当前计划、实施仪表与摘要见[全 Rust 改造方案](optimization/2026-10-04-all-rust-backend-plan.md)和[执行记录](optimization/2026-10-04-all-rust-execution.md)。
