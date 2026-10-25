# Iteration 157：IMS 入站短信解码与 N 待耐久入库合并到 Rust

日期：2026-10-08。所有改动及测试产物均在 `/Users/proletarius/Desktop/Switch`。本轮不生成 DMG、不访问读卡器/SIM、也不建立真实 IMS 或运营商连接。

Rust 现可通过现有认证私有 `POST /api/internal/ims/sms-transactions` 的 `decode_and_store_incoming_delivery` action，一次完成入站 RPDU/TPDU 解码、短信事件组装与 profile-scoped N 待入库。Rust 把原始 RPDU/TPDU、内容、时间、分段信息和状态报告写入现有 Rust 多待账本，并在耐久提交成功后返回 `store_state`。磁盘操作交给有界 archive worker，避免占用异步 I/O worker。畸形 RPDU 和持久化失败都返回非成功；Go 侧只有收到非空持久化状态后才继续已有 callback/RP-ACK 次序。

Go N 待管理器删除了原来的事件序列化和 `/api/internal/multistandby/store/inbound-event` 调用。Go IMS callback 现在只验证 Rust 已返回耐久状态，并在 `undecoded` 时投影既有警告。普通读卡器 IMS 仍走 decode-only action，不会误写入 N 待。IMS SIP 200、注册 stream、SIP 事件/会话生命周期和 RP-ACK 写入仍由 Go 控制；Swift 与 UI 没有变化。本轮没有增加或删除 HTTP method/path，契约总数维持288条。

验证通过：新增 Rust decode-and-store 测试覆盖首次入箱、同 profile 原始 RPDU 重试去重、跨 profile 隔离、畸形 RPDU 不入库；Go coordinator 和 N 待警告测试通过。真实 Rust daemon + Go 子进程的离线集成 **15项**通过，覆盖持久入库、网络重试、状态报告、未解码原文保留、Go 重启后的分段短信和删除墓碑。完整 `scripts/test-migration-gates.sh` 通过：Rust daemon **930通过、3忽略**；Go 全仓 `go test`/`go vet`、2,722项载波回放、USB 状态/射频/codec/HTTP parity 均通过。迁移扫描器 43 项自测、契约 `--diff`、Rustfmt 与 `git diff --check` 通过。Route census 仍为 **288**（Rust219/Go69，含35个HEAD alias）、127个 Swift HTTP 调用点、257个审阅范围；`--require-rust-only` 仍如预期被默认 Go fallback 和145条 Go owner/dependency 路由阻断。

P3 保守计入 **+0.025计划人周**，由1.515增至**1.540/8.0（19.25%）**。累计从13.470增至**13.495/47.5≈28.4%**（约±3个百分点）；待完成约**71.6%**。P2A为68.36%，P2B为60.94%，P4为10.56%。这代表一个 N 待持久化交接已迁入 Rust，不代表单线路 VoWiFi 闭环、多 Profile 运行、真实短信或 Go-free 构建已完成。
