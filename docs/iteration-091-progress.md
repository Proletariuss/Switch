# Iteration 091：通话事实、终态归档与后台观察接入 Rust

日期：2026-10-05。继续全 Rust 后端目标；本轮所有源码、缓存、日志及合成数据都在 Switch 内。未启动生产 App、访问真实模块/读卡器、拨打电话、收发测试短信或生成 DMG。EVS→AMR、无主机 PCMA 的目标不变；EVS/AMR-WB 尚未接入，当前主机实际能力仍为 AMR-NB。

## 生产接管

Rust `CallFacts` 现在负责通话记录的活动状态、首次观察时间、方向、号码更新、接听标记、记录 ID、时长与不可变终态；直接复用 Rust `ArchiveStore` 写入。Go 不再保存 `callSessions`，不再生成结束记录；旧 `appendCall` 和记录 DTO 仅保留在兼容测试文件。私有 `/api/internal/call-archive/ingest` 兼容入口继续存在。

- 内部来源分为 `module_cs`、`module_ims`、`reader_ims`，各自持有活动槽和执行锁；公开记录仍投影为 `module` 或 `reader`。模块 CS 的迟到 idle 不会再结束模块 IMS 的电话。
- Rust supervisor 为每个 Go 进程注入独立 producer ID，确认进程退出后撤销旧 ID。每个 IMS service 在构造时取得固定 owner ID 与单调 epoch；重连保留卡片 generation，也不会复用这个通话 owner。较旧 epoch 的事实不影响新会话；同 epoch 不同 owner 拒绝。
- 每来源序号连续，同序号重试必须是原始完整内容。最后确认的事实保留 ACK 缓存；较早序号拒绝，不靠重新生成结束时间去重。IMS 终态还须匹配 Call-ID；重复 RING 不重置开始时间，空号码不清掉已有号码；匿名记录省略number，保留Swift“未知号码”的兼容显示，answered 只会变为 true，CS active 不凭空建记录，unknown 不结束电话。
- 首次终态冻结记录后才写档。磁盘/确认失败保留原始 FrozenTerminal；后续事实不能覆盖它。写盘不持有短状态锁；确认目录同步后才推进 ACK。HTTP 等待取消不会中断已经执行的落盘工作。换 Go 进程保留冻结记录，不伪造未结束电话的挂断时间。
- Go 的三个有限队列各最多 128 条，有固定单 worker；号码、观察时间、序号和发送内容在准入时冻结，重试不变。队列满、无效事实、永久拒绝均锁存错误，不覆写旧队头、不无限开 goroutine。临时 RPC 失败显示等待确认，恢复 ACK 后清除；永久失败保留异常。

## 原始观察与退出

IMS 在原状态改变处捕获历史相关投影，释放 runtime 状态锁后有序执行短队列准入。后台远端结束、超时及关闭会产生事实，无需通话页轮询；媒体计数、音频 token 和 RTP 热路径数据不进入记录队列。实际服务资源关闭后退役 owner，并屏蔽旧音频线程的迟到历史观察。N待的 SMS-only 服务不安装语音观察。

模块 CS 的 RING/CLIP/挂断等观察复用单 URC 循环，有序短队列投递，避免逐条 goroutine 乱序。CLCC 查询及 ATA/ATH 响应使用观察版本检查，旧结果不能覆盖期间的新事件；ATD 另用通话边界版本，新 RING/挂断会使旧拨号结果失效，正常 CONNECT/CLIP 或轮询不会导致已接受的去电漏建记录。CONNECT早于ATD成功时保留同一拨号边界下的实际观察时间，成功begin后补送answered；失败/新RING/挂断清除缓存，不凭空推断接听。CLCC 的 10 秒来电缓存、无号码 active 残留过滤及原设备锁仍保留在 Go。

历史故障不关闭模块、IMS、短信或语音，也不阻止 IMS 重连。后台来源保留固定诊断，通话状态响应增加可选 `history_error`，Swift 将其接入既有通话错误提示；不把实际呼叫状态误改为失败。

退出顺序为：停止公共 HTTP/IMS 等生产者及恢复 radio → 停止并等待模块循环 → 关闭事实准入并排空实际队列 → Go 确认退出 → Rust 等待实际写入并重试冻结终态。VoWiFi恢复radio失败时收集原错误并继续等待生产者退出，避免错误分支提前关桥；若总deadline已到或模块循环退出尚未确认，不关闭桥准入、不伪造正常retire，明确报告未排空。私有 observe 只记录已经发生的事实，不执行拨号或卡片操作，因此退出期允许当前可信 producer 的全部合法事实按序排空；若按开始退出的时刻拒绝 begin/answered，会卡住其后的 end，这个审查发现已修正。

## 验证与审查

- Rust daemon 首轮全量 455 项通过；最终退出策略与来源验证收紧后，`call_` 定向 26 项通过、431 项过滤（当前总数 457）。新增 16 项核心测试覆盖三来源、epoch、重复/冲突序号、写失败保留、旧终态、producer 更换与在途提交、真实任务取消、正常关闭和严格 schema；内部 HTTP 测试验证认证及退出期 begin→answered→end 落盘。
- Go 三包 race：macOS 后端 430 个顶层/714 含子用例，modem 145/241，imscore 152/257，合计 727/1212 通过；没有失败。5 项既有可选 fixture 因未配置环境而跳过：AMR、PCMA、SDP parity，N待 process fixture，SMS submit parity。本轮没有将这些跳过项算作通过，历史 PCMA 向量也不表示生产启用 PCMA。
- 最终 ATD 边界、提前CONNECT补送、Go schema及未确认退出保护后，定向 race 21 个顶层/48 含子用例通过，无跳过。最后修复恢复radio错误的提前退出后，shutdown定向race再通过8个顶层/10含子用例，无失败/跳过，覆盖真实退出前仍接事实、实际退出后才retire及取消保留错误。IMS 6 项与 CS 4 项新增观察测试在三包 race 中通过。
- Swift 生产通话隔离 fixture 通过，新增检查旧响应兼容、记录失败不改变呼叫状态、reader 结束后的错误展示及 reset；三个修改文件语法检查通过。Rust workspace 与独立 include 测试格式、`git diff --check` 通过。
- 日志位于 `Switch/build/optimization/2026-10-05-call-facts-*`：`daemon-tests.log`、`final-rust-tests.log`、`go-packages-race.jsonl`、`go-connect-race.jsonl`、`go-shutdown-final-race.jsonl`、`swift-tests.log`。

接口清单最终一致性检查通过：200条method/path（Rust100、Go100），127个Swift调用点；新增15个可解析scope，共118项，已有接线scope重新人工审阅。16项契约工具自测通过。Go manager及imscore/call_runtime的raw string超出现有通用scope词法器能力，因此仅按人工审查与race覆盖登记，不伪造自动指纹。Rust-only退出门禁按预期返回1，仅报告默认Go fallback与125条仍有Go所有权/依赖的路由；无清单或scope漂移，不将此预期拒绝描述成全Rust验收通过。契约日志为`2026-10-05-call-facts-contract-{final-write,check,selftests,rust-only}.log`。交叉审阅覆盖Rust存储/生命周期、Go生产切换及观察者边界；退出队头阻塞、正常CONNECT导致ATD观察漏建、历史故障扩大为通信故障、radio恢复错误跳过真实退出及匿名号码兼容等问题均在交付前修正。

## 尚未完成与取舍

这轮迁移的是通话记录的事实聚合，不是完整 SIP/语音状态机。SIP/媒体、CLCC 查询和 UI 状态聚合、USB/模块锁、读卡器/IMS 实体会话仍在 Go。Rust 未接入 EVS、完整语音 owner 或真实设备业务，不能据此提高 P5 到端到端完成。

活动槽、尚未获得 Rust 确认的 Go 队列和冻结记录仍是进程内状态；Rust 崩溃后恢复这些未确认事实尚未实施，不承诺跨进程崩溃零丢失。已经获得耐久 ACK 的结束记录按原归档协议保留。队列满会拒绝新的未准入事实并显示错误；不将有限队列描述成无限可靠存储。无任意历史 Call-ID 的永久集合，旧 IMS live 事件的排除依赖原 runtime 的身份检查、有序投影及 owner epoch；旧终态额外有 Call-ID 校验。

时长仍是首次观察开始到结束、包含振铃的口径；跨进程时间使用冻结的 UTC 观察时间，时钟回拨时负时长收敛为零，不冒充已测时钟校准。模块真实 URC/CLCC 时序、运营商通话、升级、崩溃恢复和长稳尚待验收。9eSIM 6982 继续排除。

完成并通过本轮回归的通话事实聚合计入P2A：59%→61%，增量0.110计划人周；总计10.905/47.5≈**23.0%**（估计误差±3个百分点）。P2B保持35.5%，P5不重复计分。原47.5人周基线未包含新增EVS平台/codec成本，其总分母待试点重估；不按代码行数或新增路由计完成率。


## 后续独立迁移范围

下一轮优先迁移USB AT自动短信收件服务：Rust持有单worker调度、刷新合并、解码/去重排序、接管/预览决策、归档和轮询状态；Go暂保留固定设备、占用锁与来源代次绑定的原始CMGF/CPMS/CMGL快照桥。保留不自动批量删除USB短信的行为，不与已迁入Rust的串口CMTI服务重复计分。离线门禁包括虚拟时钟/单事务、VoWiFi占用与拔插、旧代次及Go重启、SM/ME部分失败、长短信/损坏PDU、归档失败及实际退出；确认后移除Go生产poller，避免双重轮询。此段仅为后续范围，不计本轮完成率。
