# Iteration 314：USIM 信息刷新接入 Rust reader admission

`RefreshUSIMInfo` 现在先快照选中 reader 与 generation，在不持有 `readerManager.mu` 的情况下向 Rust Gate 申请一次性 reader operation lease；拿到租约后再重锁并核对完全相同的 reader/generation，然后沿用现有 Go `authMu` 与 Rust PC/SC 事务执行 IMSI/ICCID/应用信息读取。Rust 明确返回 `card_busy` 时，刷新在任何 APDU 前失败；Go 本地锁仍作第二道保护。租约释放绑定初始 reader/generation。没有 UDS 的独立 Go fixture 仍走旧兼容路径。

回归新增 Rust Gate busy 的身份绑定、拒绝后无 release/无读卡，以及原有 callback `authMu` 互斥测试。定向 Go 测试通过；完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1203 passed / 3 ignored**，Go vet/全包测试通过；2722 项载波回放和 USB 状态、Radio、HTTP method/HEAD、SDP、AMR、IMS MESSAGE parity 通过。API/消费者 census 保持 **336 method/path（Rust 275 / Go 61）**、127 Swift HTTP 调用点、327 审阅范围；reader manager review fingerprint 已人工复核更新。

边界：只统一该公开刷新入口的跨运行时准入，Go 仍拥有 `authMu`、USIM/APDU 执行桥与连接/替换/复位生命周期；其他 `authMu` 消费者仍待纳入 reader actor。此项不计新的加权 owner 迁移分，总体仍 **31.4%**。未访问读卡器、短信、呼叫或运营商；未生成 DMG。
