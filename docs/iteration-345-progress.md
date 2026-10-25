# Iteration 345：reader identity 元数据与 generation 原子提交

读卡器身份刷新此前分两步写入 Rust：先通过 reader lifecycle 提交卡片 generation，再由 Go 调用 `/api/internal/reader-identity/cache` 写 metadata。第二次调用失败时，Rust 已接受新 generation，但身份投影缺失，形成部分提交状态。

现在身份 metadata 随不可变 lifecycle intent 一同进入 prepare/commit。Rust 在提交时验证 metadata 是有界对象、ICCID 为规范数字串且与 current ICCID 一致，并在同一 Gate 锁内更新 generation 和 metadata。pending transition 隐藏旧快照；abort 保留旧快照；commit 后新快照立即匹配新 generation。Go lifecycle 客户端冻结同一 metadata 请求并在确认 Rust receipt 后才发布兼容缓存；丢失/不确定 receipt 继续失败关闭。独立身份缓存 handler、Go RPC 客户端、路由预算和 owner 声明已移除。

验证：reader lifecycle Rust 专项 25 项、reader operation 专项 10 项、请求预算专项 4 项、Go `cmd/proletarius-macos` 全包测试及身份提交/丢失回执专项通过。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace 格式与测试、Go 全包 `vet`/测试、Swift BLE 物理完成及 manager 回归、carrier/USB/radio/HTTP HEAD/SDP/AMR/MESSAGE parity 均通过；最终日志为[`iteration-345-migration-gates.log`](../build/optimization/iteration-345-migration-gates.log)。契约清单从 344 调整为 **343 method/path（Rust 282 / Go 61）**；127 Swift 调用点、354 审阅范围保持，契约 `--diff` 通过。`git diff --check` 通过。

计分取舍：整体仍为 **15.870 / 47.5 = 33.4%**。本轮把已由 Rust 管理的快照改为与 reader generation 原子提交，并退役一次额外 RPC；reader refresh orchestration 仍由 Go 调用，故不把一致性加固重复算作新增生产业务迁移。下一步按计划继续迁移跨 reader 的 module SIM identity prepare/read/commit 与 reader lifecycle 物理生命周期。

没有连接读卡器或实体号卡、没有切卡/下载 Profile/发送短信/拨号、没有生成 DMG。9eSIM `6982` 仍按用户要求排除；本轮只验证离线代码与固定测试夹具。
