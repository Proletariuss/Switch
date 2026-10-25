# Iteration 159：BLE Profile 删除与 N 待卡片列表投影迁入 Rust

日期：2026-10-08。所有源代码、审阅记录和验证产物均在 `/Users/proletarius/Desktop/Switch`。没有连接实体读卡器/SIM，没有建立真实 IMS/运营商连接，也没有构建 App 或 DMG。

选中 BLE reader 时，Profile 删除现由 Rust 经 Swift CoreBluetooth APDU bridge 执行。USB/PCSC 与 BLE 共用 `delete_profile_on_transport`，因此 Profile/AID 查找、Delete APDU、通知序列确认、ES9+通知投递、NVRAM空间统计和错误分类保持同一实现。操作取得 Go generation-bound mutation lease 后再次核对所选 reader；设备选择变化时释放租约并停止，租约丢失时取消后续 APDU。Go 仍负责 VoWiFi停止协调、读卡器租约/heartbeat/finalizer和缓存刷新，Swift继续负责系统 BLE bridge。未新增 HTTP method/path。

`GET /api/multistandby/cards` 的公开分组投影迁入 Rust；Go移除了该公开 handler，保留受内部token保护的 runtime-snapshot adapter，按需刷新物理读卡器清单并返回易变卡片身份、可用状态和Profile观察。Rust对每个 card_id汇总Profile数量，并保留旧响应字段。`GET /api/multistandby/profiles`继续由Rust把耐久 catalog/desired-state与实时runtime snapshot合并。Go仍拥有物理发现、多Profile会话和卡仲裁；该切片不等于N待生命周期迁移完成。

全量 `scripts/test-migration-gates.sh` 通过：Rust workspace的 `proletariusd` **937通过、3忽略**；Go全仓测试与 `go vet`、2,722项载波回放、USB状态、RF、codec和HTTP/HEAD兼容检查通过。Contract scanner的 `--diff` 与43项自测通过，清单为291条method/path（Rust223、Go68，含37个HEAD alias）、127个Swift HTTP调用点和268个审阅范围。Rust/Go隔离进程集成 **17项**全部通过，覆盖卡片与Profile投影、普通IMS短信耐久归档、N待事务和重启恢复。新增fake BLE删除测试验证完整删除APDU序列，并确认每条APDU绑定所选reader与认证token。`git diff --check`和Rustfmt通过。

Rust-only退出门禁仍失败：默认Go fallback尚在，148条路由仍有Go所有权/依赖。纯投影测试和fake bridge不代表实体BLE/PCSC、运营商VoWiFi/SMS或长稳验收。

保守计入P2B **+0.025计划人周**，由 **4.900/8.0（61.25%）**提升至 **4.925/8.0（61.56%）**。P2A维持 **3.785/5.5（68.82%）**，P3维持 **1.540/8.0（19.25%）**，P4维持 **0.475/4.5（10.56%）**。总体由 **13.545/47.5**提升至 **13.570/47.5≈28.6%**，估算误差约±3个百分点，尚余约71.4%。未生成DMG。
