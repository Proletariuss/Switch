# Iteration 313：BLE 热插拔维护服从 Rust reader admission

本轮把 BLE presence bridge probe、周期性 BLE inventory bridge 请求、以及 Rust hotplug tracker 已确认后的最终 Go Disconnect teardown 接入 Iteration 312 新增的 Rust operation lease。维护请求使用一次性、不排队准入；同 reader 被 N待、native worker 或普通托管卡操作占用时，probe 保持 indeterminate，disconnect 直接不执行。取得 lease 后仍核对 reader/generation，并在有界 BLE probe 时持有 Go `authMu`，避免遗漏于 actor 迁移中的旧 Go lifecycle 操作与探测交错。无 Rust UDS 的 standalone Go fixture 延续旧兼容路径。

增加 Rust Gate 忙碌时 presence 不得被确认、hotplug teardown 不得执行的 Go 回归测试。此前 Iteration 312 的 N待初始化步骤继续使用 reservation owner-scoped admission，避免与自身 reservation 冲突。

验证：新增/相关 Go BLE hotplug 与 reader admission 定向测试通过；完整 `scripts/test-migration-gates.sh` 通过（Rust workspace 1203 passed / 3 ignored，Go vet/全包测试和现有跨语言 parity 通过）；migration contract scanner 保持 336 method/path（Rust 275、Go 61）、127 Swift HTTP 调用点、327 审阅范围并通过；Rust fmt 与 `git diff --check` 通过。

边界：BLE presence probe 和已确认 hotplug teardown 的 Go 生命周期调用现在先服从 Rust Gate，但连接、断开/替换、身份/APDU、session lease 创建和 shutdown 等其他 Go 消费者仍未迁为 Rust reader actor，`authMu` 仍是 Go owner。因此不提高全 Rust加权进度 **31.4%**，不声称 Go BLE watchdog 或 reader lifecycle 已退出。没有访问实体卡、发送短信、拨号、运营商网络或生成 DMG。
