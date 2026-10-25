# Iteration 310：原生读卡 worker 接入 N待 reservation gate

源码核对发现，Rust 原生 AKA/身份读卡 worker 使用 `reader_aka_binding::run_owned` 的独立 semaphore，但没有进入 `reader_reservation::Gate`。因此 Rust N待 owner 建立 reservation 时，仍无法从 Gate 观察已在执行的 Rust 原生卡操作；反方向也没有 Gate 准入保证阻止 N待期间启动这些 worker。

现在 `run_owned` 复用 `reader_task::run_with_gate`：worker 在排队/执行前先取得 Gate permit，permit 随 blocking closure 保留到真实执行退出。N待 reservation 已存在时，新 Rust worker 返回 `Busy`；Rust worker 已获准后，reservation acquire 返回 `card_busy`，直到 worker 返回并释放 permit。取消仍会取消尚未开始的 blocking work；已运行任务即使等待者超时也继续持有 permit，避免虚假释放。原有 8 槽并发上限保留。

验证：新增 `native_reader_workers_share_admission_with_multistandby_reservations` 覆盖两个方向及 timeout 后 permit 保持到真实 worker 退出；Rust `proletariusd` 全量测试与 workspace 门禁通过。Go vet/全仓测试、carrier/USB/radio/HTTP method+HEAD/codec/SDP/AMR/IMS MESSAGE parity 均通过。API 契约清单保持 334 项（Rust 273、Go 61）、127 Swift 调用点、327 个审阅范围；迁移契约扫描通过。

边界：Gate 是 daemon 进程内全局 admission，不区分 worker 的物理 reader，也不能保护未进入 Rust Gate 的直接 Go APDU 消费者。Go `readerManager.authMu`、BLE watchdog、reader lease lifecycle 和业务 teardown 均仍在；因此它是双向共享准入的一步，不是 reader actor 已完成或 Go owner 已退出。总体进度保持 31.4%（约 ±3 个百分点）；未操作实体卡、未发短信/拨号、未生成 DMG。
