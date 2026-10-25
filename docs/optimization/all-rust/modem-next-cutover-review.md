# 模块后续职责迁移审查

> 后续实施状态：[Iteration 088](../../iteration-088-progress.md) 已完成公开发送编排接入 Rust，并以确定性回归验证和修复下述 CMTI 代次风险。[Iteration 089](../../iteration-089-progress.md) 进一步迁入CMTI观察、有界队列、单worker及初始化/刷新scan执行；[Iteration 090](../../iteration-090-progress.md)进一步迁入完整六步串口初始化、独立能力协商和PDU就绪门禁；Go仍提供设备采集、RF策略、scan请求、非短信URC和固定硬件桥。[Iteration 091](../../iteration-091-progress.md) 已将CS/IMS通话事实聚合和终态归档迁入Rust，并接入后台原始观察；Go仍保留CLCC/UI状态、物理呼叫和SIP/媒体。[Iteration 092](../../iteration-092-progress.md)进一步迁入USB AT自动收件编排，Go暂留固定句柄原始SM/ME快照桥，实体USB owner当时仍未迁移。[Iteration 093](../../iteration-093-progress.md)现已接入Rust USB AT句柄/事务owner和本地短信快照，删除Go原始桥；Go仍保留硬件业务、radio/AKA/eSIM/语音编排，实机验收未完成。下文保留实施前审查，不代表当前尚未实施或修复。

## Iteration 092 后续范围：USB AT 传输与事务所有权

2026-10-05实施前只读评估，以下为当时的计划；后续实现与验证以[Iteration 093](../../iteration-093-progress.md)为准。本节不另行增加完成度。当时建议将实际USB AT句柄和完整事务交给同一个Rust owner，随后让本轮Rust收件服务直接执行本地快照，退出Go原始snapshot桥。

调用面包括状态/身份查询、短信提交和存储删除、eSIM的CCHO/CGLA/CCHC、VoWiFi SIM读取/AKA、诊断/AT控制台、飞行模式/网络设置、CS通话，以及语音USB composition重启恢复。部分诊断和固定短信路由直接使用Go句柄，不能只替换通用`runATCommand`。

可复用已锁定的rusb 0.9.4及QMI实现中的发现/claim/释放、错误映射和有限读取模式；AT接口须保持原有2/3、ff/00/00筛选与AT探测，不能直接套用QMI筛选或常驻reader线程。还需核对当前ioreg只发现DJI、底层opener有Quectel fallback的差异，保持发现行为的兼容边界。

实施需同时覆盖：

- Rust独占句柄、有限队列、session/generation以及跨RPC事务lease；整组短信、SIM/APDU和存储选择不可交错。Go进程内的`moduleSIMMu`无法约束Rust本地收件，故不再作为跨owner唯一互斥。
- 语音ADB启停/重枚举取得Rust排他gate；VoWiFi启动、活动和退出恢复期间的收件禁入由同一owner确认。发现、断开冷却、重开和eSIM manager通知保留。
- 固定短信路由绑定Rust会话，维持ICCID/IMSI校验及分段过程不重连、不回退、不重放。短写和prompt后超时保持结果未确认；不能采用QMI补写策略重发短信。
- Go重启撤销其旧lease，旧会话错误不能关闭新句柄。退出必须等待实际I/O，且Rust owner在Go完成射频恢复前保持可用，不能倒置资源关闭顺序。

离线验收覆盖接口筛选、分帧/partial response、prompt/ESC、短写、排队取消、整组不交错、旧代次、语音gate、Go崩溃与实际close等待。生产Go无AT libusb句柄、全部调用统一进入Rust owner且Rust收件不再反调Go才算完成此范围；真实USB阻塞、插拔和设备差分另行验收。

## 以下为 Iteration 088 前的历史审查

日期：2026-10-05。状态：**源码只读审查；以下建议未实施、未验收，不计为迁移完成证据。**

本审查检查了 Switch 当前 Go modem manager、设备短信发送流程和 Rust modem actor 的实际接线；未启动应用、使用真实读卡器、发送短信或生成 DMG。本文仅记录观察与下一步建议，不改变全 Rust 主计划、现有进度和验收门禁。

## 建议优先级

| 候选职责 | 当前实际归属 | 实施价值 / 风险 |
| --- | --- | --- |
| 设备短信公开发送意图编排 | Rust 拥有持久化 ledger；Go 负责公开 prepare/get/list、claim 协调、外部执行、归档确认及结果投影 | 高 / 中，推荐下一项完整职责迁移 |
| CMTI 自动接收和启动扫描触发 | Rust 已负责读取、解码、归档确认及安全删除；Go 负责触发和生命周期 | 中 / 中，可随后推进；存在下述代次传递风险 |
| 全部 URC 分发 | Go 仍分发 SIM、USSD、语音、PCM 等回调 | 高 / 高，当前会同时触及多个尚未迁移的 owner |

## 推荐：公开设备短信发送编排归 Rust

目标是让 Rust 接管以下完整公开流程，同时保留现阶段必要的 Go 硬件 owner 桥。硬件桥是过渡依赖，不能称为 IMS、USB 或整个短信业务已完全去 Go。

- `POST /api/sms/send-intents`
- `GET /api/sms/send-intents`
- `POST /api/sms/send`

### 现有代码接缝

- `backend/cmd/proletarius-macos/device_sms_send.go`：`normalizeDeviceSendRequest`、`deviceSendPayloadMatches`、`public`、`prepareDeviceSMS`、三个公开 handler、`submitDeviceSMS`、错误和 outcome 映射仍为 Go 业务逻辑。建议将这部分完整迁移到 Rust 服务，而非只迁移一个字段解析 helper。
- `rust/crates/proletariusd/src/device_send_store.rs`：已有 `get/list/prepare/claim/finish`、损坏失败关闭及启动 `recover()`。Rust 服务可直接调用内部 typed 方法，去掉经 Go 发起的 ledger HTTP 往返；不要绕过现有原子写入和故障门禁。
- `rust/crates/proletariusd/src/message_archive.rs`：已有 `ingest_messages_to`，可由 Rust 直接归档发送结果。
- `backend/cmd/proletarius-macos/device_sms_send.go` 中 `captureDeviceSMSRoute`、`boundIMSDeviceSMSRoute`、`withDeviceSMSModuleReservation`、`sendBoundUSBDeviceSMS`：目前依赖 Go 持有的 session/transport 对象及应用锁，适合先作为窄内部桥保留。
- `backend/cmd/proletarius-macos/sms_devices.go`：现有设备选择与 readiness 错误语义需要等价保留。不得用 Rust 临时构造的显示标识替代实际 route reservation。
- `rust/crates/proletariusd/src/main.rs`：Go 确认退出后已调用 `device_send_store.recover()`；新服务必须同时使旧 Go reservation 失效。
- `rust/crates/proletariusd/src/ownership.rs`：公开发送路由当前仍声明 `GoCompatibility`，迁移接线与测试通过后才修改并更新契约清单。

### 过渡边界

Rust 成为唯一公开准入、账本变更、结果投影和最终归档决策方。Go 内部桥只提供固定 route 的 capture、validate、execute、release，保留现阶段必须依附硬件 owner 的锁和身份复核。桥不得自己 claim、归档、finish，也不得在 execute 失败后重新择路或回退发送。

这能减少 Go 业务职责，并让之后 Rust IMS/USB owner 替换桥时不再重写公开发送流程。保留 Go-only 回退时必须明确隔离其旧行为；不能让 Rust 与 Go 同时拥有同一公开操作的提交权限。是否在当前阶段继续支持该回退，仍依主计划和既有发布门禁处理。

### 必须保持的等价语义

1. 同一 request ID、相同 payload 的并发请求只允许一次外部执行；phone/message/device/subscription 冲突在 I/O 前返回 409。
2. 只有 claim 已持久化且明确返回提交许可，才执行硬件调用。claim 回复丢失不能重试提交。
3. 在 claim 前、claim 后和等待模块锁后检查固定 route；实际外部提交前再次确认，不允许换设备、换卡或自动回退。
4. 外部执行部分成功、ACK 丢失、归档失败或 finish 失败均不得恢复为可自动重放状态。
5. 外部请求断开后仍允许完成只涉及持久化的 finish；取消不等于短信未发送。
6. 公开查询把 `submitting` 投影为 `unknown`，不暴露 `binding` 或 `attempt`。
7. 只有 accepted 且 archive confirmed 才返回 `sent:true`；保留原错误码及可选 `recoverable` 字段（false 时省略）。网关拒绝重放的错误另保留 `retryable:false`，不能把它加到所有 Go 业务错误中。
8. Go 重启撤销原 route reservation；prepared 旧 binding 无法继续提交；Rust ledger 的 submitting 恢复为 unknown。
9. 无 request ID 的旧客户端每次显式 POST 仍生成新意图；保留设备未就绪、多设备未选择等错误语义。
10. 使用 `SendRequestID` 保留同毫秒、相同文本的两次独立显式发送，不错误合并档案。

### 离线验收建议

迁移 `backend/cmd/proletarius-macos/device_sms_send_test.go` 已有故障和并发矩阵到 Rust fake route executor：同 ID 并发与公开 ACK 丢失、claim 写失败和 ACK 丢失、network/archive/finish 失败、payload/subscription 冲突、进程重启和 route 失效、查询脱敏及同毫秒两次发送。

保留并验证 Go 窄桥已有测试：reader generation 变化、USB 身份与 transport 变化、锁等待后身份失效、多发送事务不能交错，以及 VoWiFi starting/retiring 与蜂窝发送互斥。桥迁移之前不能仅用 Rust fake 测试宣称这些硬件约束已接管。

增加 Rust 公开 HTTP handler + fake Go Unix socket 的跨边界测试，特别验证 execute ACK 丢失后不再发第二次硬件请求。重复使用 Rust ledger 的 recover、原子写入、损坏失败关闭测试。最后复核 route ownership 和契约清单。

上述测试建议尚未执行；本审查不提供真实短信、完整 IMS/USB 或 Go owner 退出证据。

## CMTI 代次传递风险

源码观察：`backend/internal/modem/manager.go` 的 `handleURCObservedWithEvents` 收到 Rust frame 观察到的 `SMSGeneration`。其 CMTI 分支在调用 goroutine 时只传 `storage/index`；`backend/internal/modem/sms_receive.go` 的 `readAndProcessSMSFromStorage` 在稍后执行时重新读取 `m.smsEpoch.Load()`，再交给 Rust `ingestStoredSMS`。

因此，若旧卡的 CMTI 在 Go 调度中延迟，而 SIM/RDY 事件先更新了代次，旧通知可能按新代次读取相同存储和索引。**这是基于代码的竞态风险分析，尚未通过确定性复现测试证明发生；不可在进度记录中声称已修复。**

建议原生接收 worker 在 Rust `frame_input` 首次观察通知时冻结 `(storage, index, generation)`，经受限队列交付；不得等 worker 开始后重新获取当前 generation。Rust 的 stored read、归档前 generation 确认及删除 fence 均继续验证这个原始代次。测试应暂停旧 CMTI 的消费，注入 SIM 代次变化，再恢复消费，确认不按新卡代次读取或删除旧索引。

现有接缝：

- `rust/crates/proletariusd/src/modem_actor.rs`：`frame_input` 已给每个 frame 附带观察时 `sms_generation`；`ingest_stored_sms_with_store` 已拥有 snapshot、解码、归档和 generation/storage/PDU 验证删除。
- `backend/internal/modem/manager_sms_storage_test.go`：现有测试确认 CMTI 和 `CheckAllSMS` 调用 Rust ingest；尚未覆盖上述异步代次变化。
- `backend/internal/modem/manager_execute_at_test.go`：已有 typed CMTI 通知无需 Go 重解析即可到达回调的测试。
- `backend/cmd/proletarius-macos/main.go`：`manager.Start()`、初始化/默认无线策略后启动 `CheckAllSMS()`；原生 startup scan 必须保持初始化次序，不能在 serial open 时立即扫描。
- `SetDisableURCRead` / `SetNewSMSHandler` 在本次全仓检查中仅见定义及测试，未见生产调用。仍需在迁移时明确其兼容策略，避免两个接收者同时触发读取。

该风险可以独立补充确定性回归测试，但长期目标应是 Rust 自主拥有接收事件与生命周期，从实际流程中移除 Go 的触发往返，而不仅增加另一层回调。

## Iteration 094 后续接管边界审计

2026-10-05源代码复核：本轮选择完整USB设备状态采集（固定AT计划、同一session事务、解析与射频关闭投影）进入Rust，作为后续射频策略的固定设备观测基础。选择依据是当前状态采集对每条命令分别获取/释放占用，既增加跨进程调用，也可能组合不同句柄代次的字段；本轮不把迁一个CFUN helper当作完整RF迁移。实际实施和离线证据另见对应迭代记录。

后续完整RF owner必须同时覆盖：`module-flight-mode.json`单写者、启动默认RF-off及3秒持续执行、GET/POST结果与偏好持久化顺序、设置后回读/回滚、VoWiFi临时radio lease的启动/重连复用/观察缓存/最终恢复、恢复确认后的短信inhibit释放，以及transport执行入口的蜂窝探测抑制。Go当前`moduleFlightModeMu`还与VoWiFi启停登记、拨号、模块重启和Profile切换协调；Rust应获得真实、代次绑定的准入，而非消费一个可过时的busy快照。serial actor还需跨多命令预留。退出需先停止并join策略执行器，再完成临时radio恢复，最后关设备；当前Go monitor仅取消循环而没有join在途apply，不能照搬成“已停止”。这些范围本轮未实现，不增加完成度。

手动USB短信存储操作需在接管时修正以下源码风险，不能原样迁移：`clearUSBATSMSMemory`/物理索引删除/按发送方物理删除仅核对CPMS/CMGD的传输错误，未确认完整AT成功终态；CPMS返回ERROR时有可能继续对先前存储执行删除。`deleteSMS`的物理索引分支在硬件删除后调用`deleteSMSArchiveSender`，扩大了单条操作的本地历史删除范围。存储索引还缺少针对当前session和预期PDU的复读校验，旧索引可被新记录复用。这些是源码路径审计结论，本轮没有执行真实删除，也不宣称已完成修复或实机复现。

该存储迁移的完整终点应包含SM/ME白名单、选择/回读确认、当前session和预期记录匹配、完整CMGD确认与未知结果不重放、精确stableID归档/预览清理，以及取消后真实I/O仍持有事务。不能借用串口接收的“归档后安全ME清理”接口替代所有用户删除，因为两者的选择授权、存储范围和归档前提不同。RF和手动存储这两项继续保留在全Rust范围内。


## Iteration 095：手动存储接管结果

上述手动存储风险已在本轮实现中处理：Rust执行完整usage/clear/delete/delete_sender事务，校验CPMS选择与回读，删除凭据绑定卡身份、session及完整分段PDU来源，逐项复读后确认CMGD并恢复原选择。Go本地清理改为冻结ID集合，不再因单条物理删除扩大到同发送方历史；native预览在原生命周期锁内清理，释放lease后不重复按旧ID清新会话。私有发送方来源提示覆盖UI预览之外的记录，部分快照要求刷新，不误退本地成功。

这属于离线实现和验证证据；真实模块兼容、卡片删除和插拔/阻塞尚未验收。RF owner的偏好单写者、生命周期协调、定时器真实退出与最终恢复仍未接管，继续按上方边界推进。详细测试与限制见[Iteration 095](../../iteration-095-progress.md)。
