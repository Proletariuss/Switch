# 设备短信公开发送迁移：契约与定向测试审查

> 后续实施状态：本清单对应的公开 Rust 编排及 Go 窄桥已于 [Iteration 088](../../iteration-088-progress.md) 接线并完成离线验证。下文保留实施前审查基线，不作为当前归属说明或实机通过证据。

日期：2026-10-05。范围：只读检查当前 Rust ledger/archive 与 Go 设备短信编排；本文件是迁移审查清单，不代表新 Rust 发送流程已经实现或通过验收。未运行 Cargo、启动应用、访问真实设备或发送短信。

## 最容易漏掉的四个边界

1. Rust 应直接调用已有 ledger 原子操作，并通过 `archive_io::run` 执行阻塞存储工作。不要为了复用重新请求自身 `/api/internal/device-send/*`，也不能每个操作重新 `Store::open`，否则会丢失 `GuardedArchive` 的共享故障锁存及已观察文件存在状态。
2. `message_archive::ingest_messages_to` 直接调用 `ingest`，**不会执行 HTTP ingest handler 的 `valid_sms_record` 校验**。发送归档构造器必须保证字段及 RFC3339 时间正确；`created_at_ms` 不能直接作为 `timestamp` 的数字值。
3. ledger claim 成功是唯一外部提交许可。Rust future 被取消不意味着已经进入阻塞池的 claim 没有落盘；取消、超时和桥接回复丢失均不能自动再 claim/execute。执行开始后的 outcome、归档和 finish 需要独立于公开 HTTP 连接收尾。
4. public DTO 与 ledger DTO 不同：所有 prepare/get/list 响应都要去掉 `binding`、`attempt`；`submitting` 只在展示时投影为 `unknown`。不能在查询时把底层状态写成 `unknown`，否则合法在途 finish 会冲突。

## 文件与精确职责

| 接缝 | 当前位置与契约 |
| --- | --- |
| ledger 读写 | `rust/crates/proletariusd/src/device_send_store.rs`：`Store::apply`、`read`、`recover`；`prepare/claim/finish` 全部走同一 GuardedArchive 原子更新 |
| 阻塞与取消 | `rust/crates/proletariusd/src/archive_io.rs::run`、`blocking_pool.rs::BlockingPool::run`；排队取消可撤销工作，已经运行的阻塞任务不会被中断 |
| 存储故障 | `sms_store_health.rs::GuardedArchive::checked`；串行发布故障，后续 claim 不能越过已发生的不确定落盘结果 |
| 归档 | `message_archive.rs::ingest_messages_to` → `ingest` → `ArchiveStore::mutate_if_changed`；必须等待 durable 成功才可标记 archive_confirmed |
| 原公开准入 | `backend/cmd/proletarius-macos/device_sms_send.go::normalizeDeviceSendRequest`、`prepareDeviceSMSHandler`、`deviceSMSIntents`、`sendDeviceSMSHandler` |
| 原提交次序 | 同文件 `submitDeviceSMS`：route 校验 → claim → 再校验/执行 → 成功才归档 → detached finish → outcome |
| 原错误映射 | 同文件 `writeDeviceSendError`、`writeDeviceSendOutcome`；外层公开 code 不是 ledger 内部 code |
| 实际 owner 桥 | 同文件 `captureDeviceSMSRoute`、`boundIMSDeviceSMSRoute`、`withDeviceSMSModuleReservation`、`sendBoundUSBDeviceSMS`；仍需 Go session/transport 身份与锁 |
| 归档身份 | `main.go::receivedSMS`、`legacySMSCacheKey`、`smsCacheKey`；Rust 对应 `message_archive.rs::cache_key` |

## 准入、解码与查询

Go `decodeJSON` 在 `main.go` 中限制请求体为 1 MiB，禁止未知字段、禁止第一个值后跟第二个 JSON 值。解码错误是 HTTP 400 的 `error` 响应；已解码但字段不合法是 400 `sms_fields_required`。不要套用 archive handler 的 16 KiB 限制：合法短信正文可达 64 KiB，转义后的请求体也可能明显更大。

- phone 用 Unicode 空白 trim；device trim 后转小写；**message 仅检查非空白，不 trim、不改换行**。
- phone 最大 128 **字节**、message 最大 65,536 **字节**；client/expected-subscription token 是 ASCII `[A-Za-z0-9_.:-]`，长度 1..128 字节。
- prepare 必须提供 client_request_id；send 允许无 ID 的旧调用者，每次 POST 生成新 ID。显式 ID 的 send 只读取既有 intent，缺失返回 404，不能隐式新建。
- payload 匹配时 device/expected_subscription_id 为空是未指定约束；非空必须与保存值相等。不能从当前卡补写旧 record.subscription_id。
- prepare 先查既有 ID，同 payload 直接返回；只有真正不存在才 capture。旧 intent 不能因硬件重启/离线被重新绑定。
- GET 只接受 `client_request_id` 和 `cursor`；任何未知键、任何重复键均 400 `invalid_request`。非空 ID 和非空 cursor 互斥；空值是未指定。cursor 非空必须是合法 token。
- ledger `apply("list")` 自身只检查 cursor 长度；公开层仍要做完整 token/未知键/重复键检查。不能直接复用普通短信列表 `parse_sms_query`，它具有另一套宽松语义。
- 列表按 ID 字典序、cursor 独占，100 条一页、最多预读 101 条；末页 `next_cursor:null`，空结果 `intents:[]`。不能按 created_at 或 updated_at 重新排序。

JSON 的边角兼容要显式决定并用测试锁定：Go struct 解码接受大小写不敏感字段名、相同字段重复时按顺序覆盖，字符串字段的 `null` 不覆盖已有值；普通 serde struct 的重复字段/null 行为不同。安全拒绝可作为明确收紧，但不应误称字节级等价。Go `URL.Query()` 还会丢弃错误转义的键值；Rust form-urlencoded 通常保留或有损解码，迁移测试应明确对非法 `%`、分号的处理，不让解析差异变成重新发送授权。

## 并发、route 与取消

| 场景 | 必须保留的结果 |
| --- | --- |
| 两个 prepare 同时 get 404 | 可产生两个临时 route；只有 ledger 最终采用的 binding 保留，输家仅 release 自己创建的候选 binding |
| 同 ID 并发 send | 最多一个 claim 返回 can_submit=true，最多执行一次；其余直接投影既有状态，不重新执行 |
| claim 写失败或回复不确定 | 不执行硬件；不能重试 claim 以猜测许可 |
| claim 后 route 变代、取消或执行失败 | finish 为 unknown，允许记录已确认的部分 segments；不能回 prepared、不能自动切卡重发 |
| execute RPC 超时/ACK 丢失 | 按未知结果处理，不能重连后再次 execute；Go 桥必须单次消费绑定权限 |
| 同绑定重复 execute | 即使被错误调用两次，桥仍应只让一次消费进入物理发送 |
| Go 重启 | 内存 routes 丢失；旧 prepared 不能重 capture；原 submitting 经已确认 owner 退出后的 recover 变 unknown |
| HTTP 断开 | 未授权外部提交可取消；已经提交的结果处理、归档及 finish 继续收尾，不把连接存活当作发送许可 |
| shutdown | 阻止新 prepare/claim/execute，允许在途 finish；不应在仍有真实提交活动时调用 recover |
| 公开查询 | 只验证 route/投影，不捕获新 route、不发送；can_submit 必须同时满足 prepared 和旧 binding 仍有效 |

旧 Go 对 claim 后执行前检查 `ctx.Err()`；实际 route.send 一旦开始，不再依赖公开请求上下文。Rust 若整体 `tokio::select!` 在 disconnect 时丢弃执行 future，可能使最终 ledger 长期停在 submitting；至少要把已授权的一次执行与收尾放到有生命周期管理的后台任务，并在执行前再检查取消状态。后台任务也不能因客户端取消而新发第二次执行。

route 桥仍需保持所有硬件锁与代次复核：reader session/reader generation、modem ObservedSMSGeneration、USB 实例与观察到的 ICCID/IMSI/Inserted、USB 准备有效期、module VoWiFi starting/retiring owner。等待 `smsSendMu → moduleSIMMu → voiceUSBTransition.RLock` 后重新校验。Rust fake-route 测试不能替代这部分 Go 现有测试。

## 归档字段、时间与确认

成功发送的 incoming Value 应由固定 intent 构造，不从当前设备状态取值：

| 字段 | 值 |
| --- | --- |
| sender | 已保存 phone，表示会话对端 |
| content | 已保存 message，原样 |
| timestamp | 已保存 created_at_ms 转成 RFC3339 时间字符串，保留毫秒精度，不取 finish 时刻 |
| direction | `out` |
| device | 已保存 `reader` 或 `dji` |
| subscription_id | 已保存的值；空值可省略，不从当前卡补填 |
| send_request_id | 已保存 client_request_id，必须保留 |
| id / archived | 由 `ingest` 计算/设置；不要伪造已确认归档状态 |
| module_index / module_storage | 发送记录应为空/0，不能借用当前模块接收存储位置 |

`cache_key` 由 device、sender、content、**原始 timestamp 字符串**组成；合法 subscription 增加作用域；direction=out 且 request token 合法时再加 `send-request-v1`。这不是仅以 request ID 为主键：同一毫秒两个不同 ID 必须保留两条，同一 ID 重试必须复用相同时间文本。UTC 与 +08:00 表示同一时刻但摘要不同；新 Rust记录可以确定性使用 UTC，不能重写已有档案的时区文本或在重试时切换格式。建议覆盖整秒、120ms、1ms 和带 offset 既有记录；u64 毫秒超出 time 支持范围应返回归档失败而非 panic/换成 now。

`ingest` 成功返回 0 也表示已经存在并确认目录 fsync，必须视为 archive_confirmed=true。其保留上限是最新 1,000 条；新 incoming 若因时间过旧会被裁掉，整个操作返回错误而非成功。长时间停留 prepared 的旧 intent 发送后可能触发这个分支，仍要保持 accepted+archive_confirmed=false、禁止重发。

不需要为了短信中心可见再经 Go mergeSMS：Rust `GET /api/sms` 已先读取 durable archive 再合并 volatile snapshot。若仍发布 snapshot，必须避免持久写成功前发布“已发送”副本；发送记录也不应进入入站转发/通知流程。

## 公开结果与错误映射

| 条件 | 公开 HTTP / code |
| --- | --- |
| intent 不存在 | 404 `sms_intent_not_found` |
| payload/expected subscription/binding 冲突 | 409 `sms_intent_conflict` |
| 原 route 不再有效（claim 前） | 409 `sms_intent_binding_expired` |
| 普通 ledger 读写/容量错误，尚未提交 | 503 `sms_intent_store_unavailable` |
| accepted 且 archive_confirmed=true | 200，`sent:true`、segments、device、client_request_id |
| accepted 但归档未确认 | 502 `sms_send_archive_outcome_unknown` |
| unknown/submitting、发送错误或 finish 失败 | 502 `sms_send_outcome_unknown` |

上述 Go 业务错误使用 `codedErrorPayload`：不可恢复时省略 `recoverable`；device selection 的 `sms_device_not_ready` / `sms_device_unavailable` 是 503 且 `recoverable=true`，`invalid_sms_device` 是 400，双设备未选择是 409 `sms_device_required`。原审查把这些字段写成 `retryable` 不准确，现按真实 wire 更正；`retryable:false` 仅保留网关恢复期等原有错误契约。不能把所有 bridge 错误都转换为 intent conflict。

main 已有的 `sms_store_rejection` 会优先对故障锁存返回 `sms_store_unavailable`、reason 和 restart_required，不能被新 handler 绕过。直接调用 apply 遇到内部 `send_intent_not_found/conflict` 时仍应映射上述公开 `sms_intent_*`；不能把内部状态、binding、attempt 或完整错误对象回显给 App。

finish 只接受 accepted/unknown、segments 0..1024，其中 accepted 必须 ≥1，archive_confirmed=true 只能配 accepted。重复 finish 只有 attempt 和终态全部相同才幂等；迟到 attempt 或试图把 unknown 改 accepted 均冲突。桥返回成功但 segments=0 必须作为无效/未知结果，不能返回 sent:true。

## 建议新增的 Rust 定向门禁

以下是新公开编排需要覆盖的测试，不要求重复已有 store 单测。

| 测试建议 | 故障注入与关键断言 |
| --- | --- |
| `same_id_concurrent_send_executes_once` | 16 个并发公开请求；fake bridge execute=1、archive=1；丢弃第一份公开响应再查询/重发不执行 |
| `prepare_race_releases_only_unused_binding` | 同 ID 两次 capture；winner binding 不被 loser release，变 payload 则 409 |
| `known_intent_does_not_recapture_or_select_device` | 既有 ID、Go 离线或当前设备不同；prepare/get 仅返回既有；显式 send 缺 ID 返回 404 |
| `claim_failure_and_cancel_never_authorize_send` | claim 落盘前失败/执行中 future drop/成功但响应被丢弃；execute=0，重试不得重放 |
| `execute_reply_loss_persists_unknown` | fake Go 已消费执行后断开 socket；最多一次 execute，unknown，不重试网络调用 |
| `disconnect_after_dispatch_still_finishes` | 取消公开 handler 后 fake execute 完成；确定 ledger 最终 accepted 或 unknown，无新增外部发送 |
| `post_claim_outcomes_remain_non_replayable` | 部分发送、archive失败、finish失败分表驱动；分别验证 unknown、accepted未归档、公开不同502 code |
| `public_projection_hides_capabilities_without_mutating` | prepared/submitting/accepted/unknown × route有效/失效；无 binding/attempt，submitting原始 ledger 仍可合法 finish |
| `outgoing_archive_matches_fixed_intent` | 发送期间改“当前卡”；归档仍使用旧 device/subscription/created_at/send_request_id，消息正文不被 trim |
| `same_millisecond_different_requests_remain_distinct` | 同时间、同文、同号码两 ID → 两条；同 ID 相同序列化重复 ingest 返回0且视为归档成功 |
| `archive_capacity_or_timestamp_failure_does_not_resend` | 1,000 条比 intent 新的记录/无法转换的时间；accepted未归档、502、不执行第二次 |
| `public_json_query_and_error_contract` | UTF-8字节边界、未知/重复字段、null、尾随对象、1MiB、重复query、非法token、末页null、100/101条分页 |
| `store_failure_latch_survives_direct_calls` | 同一 Store clone 的已确认损坏/删除/写失败后排队claim不能成功，不允许重新open绕过 |
| `shutdown_denies_new_work_but_finishes_inflight` | shutdown后prepare/claim/execute为0；已经执行的一次收尾可以写入；无能力泄漏 |

可直接复用的现有证据包括 `device_send_store` 的并发claim、payload/binding/late attempt、持久化失败、分页、corruption、shutdown tests，以及 `message_archive` 的 scoped ID、幂等、容量拒绝、offset排序 tests。Go `device_sms_send_test.go` 的 reader/USB代次变化、模块锁后复核、CMGF/CMGS串行、VoWiFi生命周期互斥仍应保留为实际 owner 桥门禁。

本审查只新增此清单；生产源、测试实现与测试结果由本轮实施任务另行记录。
