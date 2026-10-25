# Iteration 088：设备短信发送编排归 Rust，语音策略更正为 EVS/AMR

日期：2026-10-05。所有修改、缓存和离线测试数据均在 Switch 内。未启动生产应用、连接号卡、发送真实短信、拨打电话或生成 DMG。

## 本轮完成

Rust 现在处理公开 `POST /api/sms/send-intents`、`GET /api/sms/send-intents` 和 `POST /api/sms/send`。请求校验、意图查询/准备、耐久 claim、提交结果归档、finish 和公开结果投影由同一 Rust 服务承担，复用现有账本与归档格式。Go 仍持有硬件和部分 IMS 会话，因此新增仅本机认证可用的固定路由桥，提供 capture、validate、execute、release；桥自身不 claim、不归档、不 finish，不重新择路或重试发送。旧 Go-only handler 暂留作兼容路径，Rust 公共网关不会再把这三条路由转给它。

- 同一 ID 与内容的并发请求只有一次外部执行许可；非空 ID 发送前必须已经准备，不因找不到记录自动建立新意图。
- claim 未得到明确耐久确认、执行响应丢失、部分提交或 finish 未确认均不重放。HTTP 等待者退出后，已开始的提交由有界 supervisor 继续完成记录；取消前未开始的操作不提交。
- ledger/归档等待预算为 8 秒，已运行的阻塞存储任务保持占用直至真正结束。claim 超时即使晚到写入成功也不授权外部执行；16 个 supervisor 的预算覆盖实际任务存活期。另以独立 16 个 claim worker 等待实际结果，通过 result/ACK 双握手移交许可；未获 ACK 时，仅本 attempt 确实赢得许可才释放固定路由，不误撤并发赢家。worker 预算持续覆盖迟到落盘及清理 RPC。
- Go 窄桥原子消费执行许可。未获 Rust 验证的孤立 capture 在 5 分钟后回收；成功 validate 后保留原路由生命周期，不额外限制用户已经准备的意图。原 USB 30 秒限制、卡片/会话代次及 Go 进程退出仍可撤销路由。
- 保持原设备选择错误的 HTTP 状态、code 和可选 recoverable 字段；保留 message 空白、请求 ID/订阅匹配及旧客户端无 ID 的每次显式发送。公开结果不暴露内部 binding/attempt。
- Go 恢复期间 POST 仍拒绝写入；GET 可以查询 Rust 中已有记录，但无法验证路由时 `can_submit=false`。这是有意保留恢复查询能力的网关行为，不意味着 Go 设备已经可用。

同时修复 CMTI 异步代次传递：收到通知时冻结 Rust frame 的 SMS generation，原样交给延迟读取任务；旧 Go fallback 在获得存储锁后先复核原代次，避免旧通知按新卡的存储索引读取。显式 generation=0 也不会被当前 Go epoch 覆盖。Go 仍负责事件触发、启动扫描时序及 URC 分派，未把这项修复计成完整接收 owner 迁移。

## 最新语音政策

按用户纠正，目标为 **EVS 优先、AMR 次选，不使用 PCMA**。此政策覆盖 Iteration 087。当前主机路径恢复真实 AMR-NB mode 7 编解码、octet-aligned/bandwidth-efficient RTP 和资源释放；Go 与 Rust SDP 同步拒绝 PCMA。EVS、AMR-WB 尚未实现，因此不会提前宣告能力。SDP 生成前验证本机 AMR codec 能成功创建；失败时拒绝建立媒体并释放分配的资源。通话页已同步显示当前 AMR-NB 与 EVS 待接入状态。

后续 EVS 可参考 Celmux 的协议层并通过来源核验的 native codec FFI 接入；EVS 的 AMR-WB IO 不再被政策禁止，但仍须编解码和模式切换验证。模块原生蜂窝语音由 DSP/固件协商，本次未声称已控制其编码优先级。现有 PRACK/UPDATE 检查仅是兼容门禁，不是完整媒体重新协商。PCMA 纯函数历史夹具仍保留，不用于当前主机通话。

## 离线验证与审查

- Rust daemon **409** 项、call crate **26** 项测试通过，包含新增 **17** 项短信编排测试。覆盖实际公共 HTTP/Unix socket 接线、认证、Go 恢复写入门禁、并发 claim、孤立 capture 释放、执行 ACK 丢失、归档失败、HTTP 取消后落盘、存储锁超时及损坏账本。最终全包日志：`build/optimization/2026-10-05-device-send-claim-cleanup-tests.log`；补强确定性“收到 claim 结果后 ACK 丢失”分支后，单项日志为 `2026-10-05-device-send-receipt-tests.log`。初次独立 claim 全局预算使并行测试 fixture 互抢名额，随后保持生产共享预算、为隔离 fixture 注入独立预算，完整重跑通过。
- Go 设备短信桥/旧发送意图/长操作响应 deadline 定向 race **27 个顶层测试、含子用例 58 项通过**。虚拟时钟验证孤立 capture 过期、确认后 10 分钟仍可一次执行和底层路由失效拒绝。日志：`build/optimization/2026-10-05-device-sms-route-bridge-race.jsonl`。
- Go modem 全包与 CMTI/旧代次存储读取定向 race 通过；三个新确定性用例覆盖异步队列代次变化、显式零代次及旧 fallback 锁等待。此为隔离数据测试，不是实卡接收验收。
- Go imscore 全包与语音策略定向 race 通过，Go/Rust **49** 项 SDP 差分通过；两种 AMR RTP 封装的生产编码/解码本地双向音频回环通过。没有以纯 framing 夹具替代编解码验证，也没有真实运营商通话证据。Go/Rust 差分和回环结果保留于本轮工具执行记录。
- Swift 通话页语法解析通过，未进行完整 App 构建/运行验收。日志：`build/optimization/2026-10-05-evs-amr-swift-parse.log`。既有两项 native VoWiFi dead-code warning 未在本轮处理。
- 三方交叉检查覆盖公开 Rust 编排、Go 硬件桥、持久化和 CMTI 边界；修正了错误字段兼容、恢复期间写入、存储等待预算、迟到 claim 清理以及确认后意图不可任意过期的问题。最终契约扫描通过：**197** 项接口（Rust **97**、Go **100**）、**127** 个 Swift 调用点、**81** 处人工审阅指纹；工具 **16** 项自测通过。Rust-only 退出门禁仍因默认 Go fallback 和 **122** 条 Go owner/依赖记录按预期拒绝。日志：`2026-10-05-device-send-contract-write.log`、`2026-10-05-device-send-contract-check.log`；这些是源码一致性证据，不是完整 Go-free 证明。最终 rustfmt 与 `git diff --check` 通过。

## 进度与剩余边界

本轮把公开短信管理/发送编排的实现及离线证据计入 P2A：57% → 59%，增量 0.110 计划人周。原 47.5 人周基线的精确加权和为 **10.667 / 47.5 ≈ 22.5%**，估算误差仍约 ±3 个百分点。此前 10.562 使用了已四舍五入的 P2A 贡献 3.14；按 57%×5.5 得到 3.135，旧精确和应为 10.557。本轮修正该 0.005 的显示误差，不把它算作工作增量。

语音政策修正与 CMTI 缺陷修复不额外增加迁移得分。EVS 新增成本尚待 macOS codec 试点后重估，22.5% 只对应原基线。Go 仍持有线路捕获、会话/物理设备协调、IMS/读卡器/N待生产发送和 URC；本轮并非设备短信端到端零 Go。原生 IMS/IKE 在线闭环、完整硬件 owner、EVS 媒体、实体卡/运营商及长期稳定性验收仍需继续，9eSIM 6982 保持排除。
