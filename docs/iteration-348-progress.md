# Iteration 348：手动 AT 控制台直接由 Rust 执行

正常 Rust 网关中的 `POST /api/at` 过去先由 Rust 验证命令和射频策略，再通过认证内部接口绕经 Go `moduleSIMMu`，最后回到 Rust 模块射频/串口 owner。现在 Rust `module_radio::Service` 持有诊断预约、验证飞行模式和 VoWiFi 冲突，并直接将命令交给既有 Rust USB/serial transport 与 modem actor。删除 `/api/internal/at-console/execute` Go handler、路由和桥接测试；显式 standalone Go 兼容启动保留原公开处理器。

保持命令语法、公开响应字段、模块错误分类和已知冲突文案。模块命令提交后遇到不确定响应时不自动重放，错误仍返回给用户；按既有手动控制台语义释放临时预约，让用户检查设备状态后决定下一步。Rust离线fixture覆盖成功结果、模块返回 ERROR、飞行模式拒绝、VoWiFi占用、非法命令，以及未知结果不重放且不遗留预约。未进行真实模块操作或实机验收。

验证：Rust AT 控制台定向测试、未知结果专项测试和 `scripts/test-migration-gates.sh` 全部通过；Rust workspace、Go vet 与全包测试、Swift BLE 回归、carrier/USB/radio/HTTP method/HEAD/codec/SDP/MESSAGE parity 均通过。契约 scanner `--write-inventory` 与 `--diff` 通过，最新清单 **342 method/path（Rust 282 / Go 60）**、127 Swift 调用点、353 审阅范围。

P2B +0.025 人周等值；累计 **15.945 / 47.5 = 33.5684%**，显示 **33.6%**（两位小数33.57%）。该计分只覆盖默认生产 AT 控制台从 Go 执行桥切换至 Rust actor，不包含 standalone Go 兼容执行器退出、串口 actor 启停接管、模块业务生命周期和实机验收。无 DMG。
