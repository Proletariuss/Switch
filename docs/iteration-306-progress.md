# Iteration 306：Reader lease owner 切换前置核对

审阅当前 Rust `NativeReaderSessionLease` 与 Go `readerManager` 的真实边界。Rust 持有 lease ID、身份/代次快照、续期请求及释放确认客户端；PC/SC 物理 lease 在 Rust owner。长时间运行的健康扫描及 eSIM 下载/改名/删除/切换处理器均启动 `NativeReaderSessionHeartbeat`，负责周期续期并在续期失败时取消工作。Go 仍持有 `readerManager.authMu`，在 BLE lease 期间负责兼容 watchdog，并用同一 gate 串行化 reader 连接/断开、presence probe、USIM/ISIM 读取、AKA handoff、LPA/profile 操作及 Rust native-session 的旧业务协调。BLE 的 Rust APDU 由 Swift CoreBluetooth 桥执行，但 lease admission 仍是 Go-owned。

因此不能独立迁移 BLE timer 或 `authMu` 的其中一端：只移 timer 会令 Rust 会话无法释放 Go 内部 gate；只移 `authMu` 则会让 Go LPA、身份读取或 reader replacement 绕过 Rust lease。P2B reader-session cutover 的首个可计分包应将 BLE 物理 reader identity/generation 与全部 operation admission 一并归 Rust owner，并让每一条 Go 兼容调用在 Rust lease token 下执行或 fail closed；旧 Go gate 需在所有消费者切换并由交错测试证明后才能删除。

本轮是 owner/consumer 依赖审计，无生产代码迁移、无实机 BLE/PCSC操作；总体进度不变。完整离线迁移门禁在 Iteration 305 已通过，Rust-only 构建退出仍未达成。无卡片 I/O、短信、通话、运营商访问或 DMG。


补充源码定位：Rust heartbeat 创建点位于 `reader_esim_download.rs` 的 health、profile mutate/download/switch 路径；续期任务实现在 `reader_session_lease.rs::NativeReaderSessionHeartbeat`。该能力已存在并由上述生产处理器使用，不应重复记为迁移缺口。
