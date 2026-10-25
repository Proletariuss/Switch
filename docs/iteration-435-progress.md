# Iteration 435：通话模式恢复准入迁入 Rust

日期：2026-10-13

Rust 接管 `POST /api/call-mode/restore` 的公开入口、请求方法/查询边界、限长 JSON body、显式 `confirm=true` 门槛，以及备份 ID 语法校验。拒绝请求不触发 Go RPC。Go 删除公开路由，只保留受认证的内部启动适配器；正式恢复前仍通过 Rust 读取并校验备份，并复核呼叫、模块及操作状态，再执行原有硬件恢复流程。

新增 Rust 路由测试和 Go 内部适配器测试；全量离线迁移门禁通过，包括 Rust、Go test/vet、Swift BLE、HTTP/跨语言 parity 和迁移契约检查。门禁日志 SHA-256：`fa8058cf9c1fa472a57c2001848a6fa8f9acd0c88aed82d3e8f4790dcb9586fe`。本轮未连接或操作读卡器、模块或 SIM。

P2B +0.025，累计 **18.020 / 47.5 = 37.94%**（估算误差约 ±3 个百分点）。API census 为 353 method/path（Rust 292 / Go 61），127 个 Swift 调用点、377 个审阅范围。Go 退出审计仍有 6 类构建/运行/源码阻塞，生产 Go 文件 266 个；没有改变 Go 运行依赖仍存在的结论。

按用户明确要求，将当前源码构建为 **3.14/build314** DMG，包内版本、签名、arm64 架构、镜像校验及归档哈希均已核实。发布详情见[3.14 发行记录](release-3.14.md)。未进行实机验收。
