# Iteration 460：Rust 原生读卡事务加入有界串行队列

Rust `NativeReaderSessionLease` 现在通过 coordinator 的单 reader transaction gate 串行执行 lease 内 AKA 与 eSIM 物理卡 worker。队列使用 FIFO Tokio mutex，并用 32 个准入槽限制活动/等待事务总量；超限沿用 `Busy` 错误。排队请求拿到锁后重新验证 lease 和 reader generation，失效或 draining lease 不会执行闭包。串行与容量 permit 随真实 blocking worker 一并转移，调用方取消或超时不会提前释放仍在运行的物理卡访问权；BLE AKA 也进入相同 admission。

新增测试验证并发事务顺序、32 项容量上限和容量归还、lease draining 时排队任务 fail-closed。定向 coordinator 生命周期测试 **21 passed**。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,463 passed / 0 failed / 3 ignored**，Go 全仓 tests/vet、Swift BLE completion/manager 回归，以及 2,722 项 carrier replay、USB/radio/HTTP method+HEAD、IMS/USSD/语音/SMS parity 全部通过。日志在 [`iteration-460-migration-gates.log`](../../build/reports/iteration-460-migration-gates.log)。`git diff --check` 和迁移合同 `--diff` 通过；当前 census 为 **354** method/path（Rust 293、Go 61，含 51 个 HEAD alias）、127 个 Swift HTTP 调用点、379 个审阅范围。

该队列仅适用于同一个已获准的 lease。它没有开放同一 reader 上的多个 profile lease、没有引入 profile 身份切换/AKA broker，也没有接管 Go `multiStandbyManager` 的 Profile session 创建、终止与物理恢复；native IMS production-start gate 继续关闭。结构审计仍有 **6 类 Go 退出阻塞**，含构建/嵌入/启动转发依赖、Go module 和 **266 个**生产 Go 源文件。故本轮是 P1/P4 资源仲裁准备，不记端到端业务迁移分；项目估值保持 **37.99%（18.045/47.5）**。没有启动 App、操作实体卡/SIM、发送短信或连接运营商网络；9eSIM `6982` 仍排除。

用户已要求的 3.15/build315 iteration460 DMG 在本轮代码改动前构建，因此不包含本轮队列改动。本轮不生成新 DMG，遵循“全 Rust 目标完成或用户另行要求时再生成”的约定。
