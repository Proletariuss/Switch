# S10 操作账本容量退役设计

日期：2026-10-23。状态：设计提案，交主代理复核后再分批实施。本文件只依据当前源码做静态推演，未运行测试，未改变生产代码、账本、GUI 检查点或验收状态。

## 1. 结论与不可同时满足的要求

最小可渐进方案是：**冻结 v1 任意 ID 的新增准入，保留其有界历史；v2 使用服务端持久签发的单调序号和有界在途槽，终态正文退役后仍由高水位永久拒绝旧序号。** 不以删除 v1 条目、扩大条数上限或清空账本解除满容量。

必须区分两项承诺：

1. 旧请求身份永不再次获得派发权：在同一耐久数据权威内持续保持。
2. 永久返回每次操作的完整原始回执：现有有限 v1 历史保留；无限增长的 v2 历史只能提供有界近期回执，退役后明确返回“身份已退役，结果正文不再保存，本次请求未执行任何新操作”。这不是成功回执，也不是允许重试，更不表示历史操作从未提交。

任意 UUID 的历史集合不能无损压缩为固定大小的精确去重集合。TTL、LRU、Bloom filter、固定数量 hash tombstone、时间戳较旧即拒绝、换 epoch 后继续接受任意 UUID，都不能同时证明无误判、无重放和无限新增。Bloom filter 可以保守误拒，但固定容量持续插入最终会丧失可用性，不作为本方案的容量闭合证明。

本方案也不宣称无限操作：序号、epoch 均使用检查溢出的有限整数；达到上限后停止签发，绝不回绕。对无限多个尚未解决的未知结果、无限客户端或任意数据目录回滚，也不存在本地固定存储、永久完整证据、持续接受新操作三者兼得的方案。

## 2. 当前代码证据与不同语义

以下位置以本次静态读取时的源码行为为准，后续改动须按函数名重新定位。

| 账本 | 当前边界 | 现有身份、派发及恢复证据 | 退役不能破坏的行为 |
| --- | --- | --- | --- |
| `esim_mutation_intent.rs` | `MAX_RECORDS=512`，`MAX_STORE_BYTES=4 MiB`（23–24 行） | `admit` 225 行写 `prepared/0`；`claim` 287 行持久改为 `submitted/1`，`execute_with_store` 481 行之后才进入操作；`recover` 190 行把未 claim 的 prepared 变成 terminal 409，已 claim 保持 unknown；相同 scope 的未决请求阻断后继 | 原 ID、action、fingerprint、完整 owner binding 不漂移；reader/module 的未知结果范围阻断不因压缩解除；未派发证明不等于允许重放原 ID |
| `reader_mutation_intent.rs` | 4,096 条、8 MiB（20–23 行） | `valid_client_request_id` 73 行当前仅接受 UUID；`Store::admit` 184 行在 dispatch 前写 submitted；`Store::open` 162 行将 submitted 转 unknown；`execute` 269 行先回放历史，之后才校验当前 inventory | 旧 disconnect 不能被重新绑定到后继 reader/generation；相同 ID 换 body/route/binding 仍拒绝 |
| `device_send_store.rs` | 10,000 条、32 MiB（16–17 行） | `prepare` 分支 176 行保存正文与线路 binding；`claim` 分支 227 行仅 prepared 首次变 submitting 才返回 `can_submit=true`；`recover_bindings` 133 行将失主 submitting 转 unknown；`finish` 246 行按 attempt 完成 | prepared 不是已经发送；submitting/unknown 永不再 grant；accepted 但 `archive_confirmed=false` 仍需恢复，不能冒充归档完成 |
| `public_mutation_receipt.rs` | `LIMIT=16,384`（13 行），`ArchiveStore::new` 实际另受默认 8 MiB 限制 | `load` 34 行要求 `sequence==entries.len()` 且 sequence 唯一；`run` 172 行先写 unknown 回执再 dispatch；`replay` 129 行允许旧回执先于当前设备校验；`latest` 298 行按 sequence 查最后准入代次 | 当前 sequence 是保存条目的排序号，不是可直接使用的客户端防重放高水位；`call_audio.rs:110` 用 latest(start) 验证 stop 所针对的准确 audio generation，包括结果 unknown 的 start |
| `native_vowifi_session/operation.rs::RequestLedger` | 每个 owner 文件 100,000 条或 32 MiB（397–400、576 行）；reader/module 各有一份 | `Options::body` 22 行要求 observe_operation 与 client_request_id；`open` 396 行将非 terminal projection 恢复为 unknown；`mark_submitted` 513 行提交后保持原身份；`lookup/insert` 535–586 行不淘汰终态；`operation/tests.rs:407` 的 `terminal_capacity_never_evicts_old_identity_and_refuses_100001` 直接构造容量边界，注明字节上限可更早触发 | 请求 receipt、真实 session owner 与 operation_id/generation 是不同层；observation_expired 不能被当作终态/物理清理证明。未来 v2 必须覆盖 reader 和 module 两个 request ledger，不能只升级其中一个 |

共同持久化基础：`proletarius-store/src/lib.rs:198` 的 `mutate_if_changed` 在跨进程文件锁内重新加载并原子提交；333–370 行实施临时文件、文件 fsync、rename、目录 fsync；非空账本与 presence witness 阻止消失的历史被当作全新空库。`sms_store_health.rs:23` 的 `GuardedArchive` 还登记耐久存在性和读写故障。新 authority 必须继承这些边界，不能用进程内 counter 或独立未协调的两个 JSON 文件代替。

当前条数限制之外还有正文与响应字节限制。单纯提高 MAX_RECORDS 既不能解决有界存储，也不能保证在到达条数上限前不会因字节上限拒绝。

第五类是本设计追加的受影响域；本次仅定位其容量、身份和恢复结构，不扩大为新的生产路径审查。上表引用容量测试的代码结构，不把本设计的静态读取表述为本代理已执行该测试。

## 3. v2 最小身份与 authority

### 3.1 显式版本，不靠字符串猜测

新增显式 v2 身份标记，例如协议 DTO 中的 `identity_version: 2`。具体字段或 header 形式在实施时统一选定；所有入口、内部 claim 和 GUI 使用同一种规则，未知版本拒绝。

示例 opaque ID：`s2:<family>:<store-id>:<epoch-hex>:<seq-hex>`。固定字段宽度和有限 family 枚举可使总长低于现有 128 字节上限；reader 的 UUID-only 验证器须显式扩展为带版本的身份验证，不能宽松接受任意字符串。GUI 不通过拆字符串自行授予权限。

`store-id` 为一次创建后耐久保存的权威身份，绑定当前账本数据域；不是每次 daemon 启动生成的 instance ID。`epoch` 首批固定为 1；不在重启、缓存清理、升级或容量不足时更换。`seq` 由服务端在文件锁内签发。

v1 eSIM/public/SMS 的合法字符串不只 UUID，也可能碰巧长得像上述 ID。因此版本解析前须处理 frozen legacy 命中；签发候选若与任一 frozen v1 ID 完全相同，应拒绝冲突或原子烧掉该序号后另选，不能把同一个旧字符串在 v2 下再执行。首批保留完整有限 v1 索引，避免以“随机碰撞概率很低”代替检查。旧 unversioned 请求缺失 v1 记录时一律拒绝，不进入 v2 自动准入。

### 3.2 一个原子 authority 快照

每个账本的 v2 envelope 至少保存：

- schema、family、store-id、epoch、`issued_high_water`、legacy snapshot 的固定路径与内容摘要。
- 固定上限的 live slots：ID/seq、route/action、业务请求指纹、原 owner/binding、状态、claim/attempt 身份、所需恢复证据；未 claim 的 reserved 也属于已烧掉的序号。
- 有界的 terminal response cache，按条数和总字节双重限额。
- 固定业务依赖 pin，例如 module audio 的最后 start ID 与 binding；pin 不依赖完整响应正文是否仍在 cache。

高水位和 slots/cache 在**同一次**耐久快照提交中改变。不能先删除旧回执再另外写高水位，也不能先返回 ID 再异步提交其 slot。`issued_high_water` 表示所有已签发序号，不要求对应条目仍存在；不得再使用 v1 的 `sequence==entries.len()` 校验式。

给定当前 family/store-id/epoch 的合法 ID：

| 请求所指身份 | 处理 |
| --- | --- |
| 匹配 live slot | 校验 route/action、指纹、binding；依原状态回放、查询或做唯一一次 claim |
| 匹配近期 terminal cache | 返回原回执；更改 payload 或 route 拒绝 |
| `seq <= issued_high_water` 且已无条目 | HTTP 410 `mutation_identity_retired`；带 `request_dispatched:false`、`retryable:false`，前者仅说明本次 HTTP 没有派发，不断言历史操作从未提交；不伪造旧物理结果，不允许“未找到所以新增” |
| `seq > issued_high_water` | 拒绝 `mutation_identity_not_issued`；只有 reserve 能签发，不直接 dispatch |
| 外来 store-id、旧 epoch、未来 epoch | 拒绝；未来 epoch 不能推动服务端升级，旧 epoch 不能再准入 |

退役后无法恢复原 payload 的 fingerprint，不再承诺区分“完全相同的过期重放”和“同过期 ID 的篡改”；二者都无条件拒绝，均无物理副作用。不能为了保持旧版的精确 conflict 分类而无限保存 per-ID 指纹。

### 3.3 reserve 自身也须防止重复签发

仅新增“服务端生成 UUID”的接口不足以解决问题：reserve 回应丢失后重试若再生成新 ID，会把同一 GUI 意图变成两次合法操作。

最小协议是带单调位置的 CAS：

1. 新的显式用户操作读取 authority status，获得 store-id/epoch/高水位和 lane 状态。该 GET 不产生执行权。
2. GUI 把候选 `seq=H+1`、intent nonce、route/action、完整业务请求字节及所需 binding **先耐久保存**为一个 pending checkpoint。
3. reserve 携带该候选位置、nonce 和请求；服务端在同一锁内仅接受当前 `H+1`，保存 slot 并提高 H，fsync 成功后返回已签发身份。reserve 不做物理操作。
4. 同一位置、同 nonce、同业务指纹重复 reserve 返回原 slot；并发 GUI 抢同位置但 nonce/payload 不同返回冲突。位置已经退役时返回 410，绝不签一个新序号替代。
5. GUI 持久保存服务端返回的 identity 与规范请求后，才允许进入现有 dispatch/claim 路径。若已持久保存候选身份可唯一导出同一规范请求，也必须读回验证后再提交。

有意新操作可以在原意图结束后读取新 H；**网络失败、410、版本升级失败和 GUI 重启均不能自动取新 H 给旧意图重新编号**。旧 reserve 的精确请求永远携带旧位置；重放不会得到新 ID。某客户端主动改位置/nonce 构造新意图属于显式新请求，不宣称能用有限账本判定两个不同 ID 的业务语义永远相同。

不引入无限客户端序号表。首批使用每个 family 的服务端全局计数器；客户端自有 sequence 只有在客户端集合也有固定上限、注册和退役均耐久、失去 checkpoint 后不自动注册新身份时才可作为后续优化。

签发 ID 不必依赖可丢失的签名密钥来证明“尚未执行”：只有命中耐久 live slot 才有 claim 资格，裸 ID、正确格式或 seq 范围本身不够。若以后加入 MAC，只补真实性，不能取代上述 slot/高水位。

## 4. 有界 slots、状态与真正可以退役的内容

必须用有限 route/lane 表，而不是为每个历史 reader、设备、UUID、客户端或 epoch 永久新建一份 counter。具体配额在实施时冻结；未知 lane 拒绝，不能自动扩表。

建议起步 lane：eSIM 为 reader/module 两个现有阻断范围；reader lifecycle 为四条已知 connect/disconnect 路由；device SMS 为 reader/dji 两个路由族；public mutation 为当前 14 条已审议路由；VoWiFi RequestLedger 按固定 reader/module owner 分开，并为 start/stop 或已审议的恢复动作保留独立有限 lane。未知 connect/start 不能占住恢复它所必需的 disconnect/stop/restore lane。业务既有卡片锁、资源 owner 和 eSIM unknown scope fence 继续优先，lane 不是绕过它们的权限。并行度若低于现状须作为明确兼容取舍，不静默改变。

状态按账本适配，公共语义为 `reserved -> claimed -> completed/unknown`：

- reserve 丢回应但未 claim：旧 ID 仍不重用；可按同 ticket 恢复，或明确终止为 not-dispatched 并烧号。
- claim 已耐久而结果不明：保持 unknown，不能因年龄、达到容量、GUI dismiss 或 epoch 变化恢复 claim 权。
- terminal 结果已持久、无后台 finalizer/归档依赖或 generation 引用：允许移入近期 cache，再按 quota 淘汰正文。保留 H 即足以拒绝原 ID。
- 正在执行、物理释放未确认、eSIM scope unresolved、SMS accepted 但 archive 未确认、被 audio generation pin 引用：不得由普通 cache GC 认作可释放。可以只保留有界的必要 pin 元数据，但必须先证明不丢恢复/ownership 义务；首批不自动精简 unknown。

达到 live slot/未知结果上限时保守拒绝新 reserve，查询、停止、恢复和已受理 owner 的 finish 仍可进行。没有独立物理结果证据就无法保证持续释放所有 unknown 槽；这是可用性前提，不能把“容量问题已解决”扩写为“任意失败历史下永不阻塞”。v1 未知记录本身仍保存在有限冻结快照内，按现行语义继续约束 v2 对应业务范围。

近期回执 cache 的 W、总字节以及单项响应限额须同时生效。W 不是永久防重放所需的 tombstone 数量。满 cache 时优先在同次快照内退役安全终态，给新 slot/finish 留出预算；若最小恢复记录都无法持久化，继续 fail closed，不强删 unresolved。大响应可先退役旧正文，不能让“physically completed 但 finish 失败”倒退为可派发。

public audio 的 `latest(start)` 是业务代次索引，不能直接改成“缓存中最新 start”。在同一个 authority 内单独保存有限 pin `{start ID,binding,issued sequence}`，stop 仍核对完整原绑定；跨 epoch 的旧 start ID 可以作为 stop 的目标引用，不能作为重新执行 start 的命令。retire/epoch rotate 之前必须证明不会切断这一引用。

GUI receipt ACK 可以减少 cache，但不是永久安全的必要条件。不能要求无限 GUI 全部 ACK 才声称可固定存储。首批采用有界近期结果协议；某个长期离线 GUI 的已完成 v2 checkpoint 若回来只见 410，保留原意图并显示结果已退役，走只读核对/明确恢复，绝不自动按成功清掉或换 ID 重发。若产品要求所有离线 GUI 永久取得原正文，则必须选择保留更多存储或停止新增，不能用 ACK 方案掩盖这个前提。

## 5. v1 迁移与旧版本边界

### 5.1 原子冻结，保留全部现有 ID

每个 family/固定 owner 独立迁移；不要求跨文件事务，但 capability status 必须逐 family/owner 声明版本，客户端不能据某一个已升级就假设其他已升级。

迁移期间持有 daemon/data-root owner 和该账本锁，等待当前受理任务及其写回 owner 安全结束或确认其恢复态；不能在活跃 owner 仍按 v1 格式 finish 时切换 schema。

1. 校验 v1、presence 及恢复状态。损坏、丢失、超限不得作为空库；不靠迁移重置 health fence。
2. 将 v1 原始有限快照保存到固定只读 legacy 路径并 fsync 文件和目录；保存其 hash 和迁移输入身份。固定 staging/legacy 槽，崩溃重启复用并核验同一份，不能每次启动多留一份备份。
3. 在**原权威文件路径**原子提交 v2 envelope：引用 frozen v1、`legacy_new_admission=false`、store-id、epoch=1、H=0、初始 pins。先提交并确认目录 fsync，才对外开放 v2 reserve。
4. 读取 v1 已存在 ID 仍按原 route/fingerprint/binding 提供原回执或 unknown；v1 不存在的 ID 返回 upgrade-required，不再写入 v1。新的旧客户端也不例外。
5. 允许恢复性补全既有 v1 ID，但只写 v2 中有界的 legacy recovery overlay，不把 frozen 文件重新变成可新增的活动库。overlay 数量最多等于原有记录；旧 ID 永远不取得第二次 claim。为避免首批出现第二套终态判定，尚无实现与测试的补全保持阻塞而非推测成功。

前四个既有快照的配置字节上限合计 52 MiB；追加两个固定 VoWiFi owner 各 32 MiB 后为 116 MiB。冻结后该部分不随新 v2 操作次数增长；另计有限 v2 quota、presence/pin、恢复 overlay 与迁移暂存。这里是文件配置上限预算，不是磁盘文件系统实测占用。若同时保留所有 legacy 原始快照及同规模 overlay，预算必须明确计入两份；首批可以只对确有恢复变化的有限记录写 overlay。已存在的外部备份/验收证据不列作可删缓存。

### 5.2 原文件必须让旧 decoder 拒绝

只把新账本写在旁边、保持原 v1 活动文件可写，会使旧 daemon 在回退后把 v2 ID 当新字符串准入。尤其 eSIM/public/SMS 的宽松 ID 验证会形成重放漏洞。

因此迁移后原文件必须是旧 `read_snapshot/decode/read/load` 以及 RequestLedger `decode_entries` 无法合法解析的明确 v2 schema；其完整文件校验失败必须先于 dispatch。冻结 v1 只供新兼容层查询，旧 decoder 不能回退到它。五类旧 decoder 都要用固定 fixture 证明拒绝，VoWiFi 两个 owner 文件分别覆盖，不能只测新 decoder。

支持的旧 App/daemon 回退范围须冻结并逐一证明拒绝新 schema；不可把“当前 decoder 会拒绝”泛化为任意历史软件都安全。安装器/启动器不得把 v2 数据目录复制成旧 v1 活动目录、删除 guard 或恢复老的高水位。代码回退能拒绝而继续运行失败，与“旧版本正常可用”是不同承诺。

### 5.3 epoch 与数据恢复

首批不实现容量触发的 epoch 轮换：单一 epoch 加 checked seq 已足以退役终态。后续若必须轮换，只能在全部 live/unknown 义务处理妥当后原子增加 epoch；仅接受当前 epoch，所有旧 epoch 全部拒绝，不保存无限 epoch 白名单。generation pin 若跨轮换仍有业务意义，必须继续保存其独立目标元数据。

目录 fsync 和 presence witness 提供当前数据根的崩溃一致性，不提供任意外部快照回滚防护。若操作者把整个数据根、presence、store-id 和 H 一起恢复到旧备份，本地代码无法仅凭相同旧字节知道已经发生过更晚的派发。强保证需要不会一同回滚的外部单调锚或受控备份恢复协议；目前未验证这一前提。检测到缺失/不一致时拒绝，不能自动新建权威或换 store-id 解锁。

## 6. GUI checkpoint 迁移

| 当前调用方证据 | 必需改动 |
| --- | --- |
| `ESIMModels.swift:379` 的 checkpoint 仅 scope/action/UUID；`begin` 439 行创建 UUID；`ReaderESIMStore`、`ModuleESIMStore` 在终态回执后 clear | v2 checkpoint 增加版本、family/store/epoch/候选 seq、nonce、业务 payload 的规范字节/指纹、reserve/dispatch 状态。不能只保存 action 后在重启时用当前 UI 重建另一个请求。历史 v1 checkpoint 原 ID 不改、先只读查询，未有证据不得自动改号 |
| `ReaderModels.swift:354` 的 lifecycle checkpoint 保存 reader/generation/inventory/instance/autoConnect，校验 ID 只认 UUID | 采用 tagged identity，保留完整原 binding；v2 迁移不能把旧 reader 的 disconnect 改绑到当前 reader。旧 checkpoint 保留 v1 分支，不能“解析失败即删除” |
| `DeviceSMSSendStore.swift:92` 的 pending 仅在内存；195 行造 UUID，服务端列表负责进程重启恢复；`needsRecovery` 32 行保留未归档 accepted | 新增先于 reserve 的耐久 issuance checkpoint，保存 payload、候选 seq 和 nonce；否则 reserve 回应丢失后 GUI 无法区分是否已获得新身份。发送记录列表与恢复状态继续由服务端主导；dismissed 仅是展示选择，不是后台 retire/claim 权限 |
| `APIClient.swift:472` 的 PublicRadio checkpoint 按固定 route 保存 original/request/id，prepare 生成 UUID，complete 只在 Rust completed 后 clear；audioGeneration 持久保留 start ID | 改成可等待 status/reserve 的准备流程，checkpoint 先耐久写再发 reserve。URLSession/async 两种发送入口都必须经过同一规则。audioGeneration 和旧 pending request 不能被新序号覆盖；不同 route 的 start/stop 恢复仍可前进 |
| `VoWiFiRequestCheckpoint.swift:4` 保存 clientRequestID/requestedEnable/完整原 payload；`begin` 31 行先写 journal，`clear` 44 行只接受匹配 Rust terminal；`write` 54 行有文件与目录 fsync | 未来 server-issued 身份先写候选 seq/nonce，再保留原 owner/generation/payload；status/reserve 协商覆盖 reader/module。不得把 client seq、operation projection Clock.sequence 或 session generation 当成同一个计数器。旧 v1 记录仍按原 ID 查询，observation_expired 不清 checkpoint |

现有 UserDefaults 的 synchronize/readback 是当前 GUI 边界；不能仅因为加入 seq 就宣称具有文件 fsync 的断电保证。v2 issuance checkpoint 应使用已有可审计的原子文件/跨进程锁能力，或单独证明替代实现的耐久契约；`PublicRadioMutationCheckpoint` 的 NSLock 只约束本进程，不能作为两个 GUI 进程的互斥证明。

GUI checkpoint 升级先以双读方式识别 v1/v2，禁止覆盖不认识或损坏的记录。没有 pending 的新用户动作可以使用 v2；现有 v1 pending 须保持原 ID 查回执/核对。只有匹配原身份的确定终态或已设计的恢复回执才能清理检查点。410、404、版本不支持、capacity-full、unknown 和 transport error 均不能触发自动重新发送。

## 7. 渐进实施顺序与每步边界

1. **共同 authority 与格式，尚不启用退役。** 实现 tagged identity、单快照 H/slots、严格 decoder、既有路径新 schema 的旧版拒绝 fixture、有限状态模型；冻结各 family lane 和 byte budget。未接生产前继续原 v1 满库拒绝，不能提前删数据。
2. **选择一个 family 做闭环。** 优先 device SMS 的既有 prepare/claim 结构作为试点，但必须同时补 GUI 耐久 issuance checkpoint、所有 native/internal claim 验证及 status/reserve；不能只改 store.apply。通过单 family 迁移/崩溃矩阵后才冻结其 v1。也可由主代理依据风险改选；public 已有 sequence 不代表它最省改动，audio generation 与同步 APIClient 准备流程均需改造。
3. **接 eSIM、reader lifecycle 及 VoWiFi RequestLedger。** 先保证 v1 pending 查询、unknown scope fence、bind-before-dispatch 和恢复 owner 不变，再启用 v2 有界终态正文退役。VoWiFi 先 reader 后 module 或反向分步均可，但各 owner 独立协商；原 operation_id/generation 与 session/cleanup 证明必须保留。未支持版本的其他 family/owner 仍按 v1 行为运行。
4. **接 public mutation。** 逐条固定路由接入；同步迁移 latest/start pin 和 audio stop 引用。raw AT 的 query-only allowlist 不因此扩大。公开与内部入口没有身份兼容绕路。
5. **最后启用 quota GC。** 先在同源测试证明 retire 只移除安全正文、H 不回退、不会丢 pin，再按小 quota 压测终态稳态循环。只完成冻结或格式升级，不算 S10 已闭合。

首批明确不做：无限结果压缩、任意旧版本无缝可写回退、数据根任意历史回滚、无限客户端注册、凭时间解除 unknown、仅根据 GUI 消失释放资源。

## 8. 迁移、竞争与崩溃测试矩阵（待实施，未执行）

全部使用工作区内临时数据根、合成 dispatcher/owner、计数器和可控 barrier；不接真实 reader/SIM，不发送 SMS/电话。断电相关 fsync 故障注入与真实进程 kill 分别记录，不能把内存 mock 等同于真实文件系统断电证明。

| 编号 | 交错或故障点 | 必须证明 |
| --- | --- | --- |
| M01 | 五类 v1 库分别为空但从未创建、非空、恰好条数/字节满；含 terminal、prepared、submitted、unknown、accepted 未归档；VoWiFi reader/module 分别覆盖 100,000 条和 32 MiB 两个边界 | 原数据逐 ID 保留；满 v1 仍能迁移，不需先删一条；有过存在记录的空/缺失/损坏库继续拒绝 |
| M02 | v1 snapshot copy 写入、文件 fsync、目录 fsync 各点故障/kill | 原 v1 仍唯一权威，v2 从未签发；固定 staging 可恢复，不累计无限孤儿副本 |
| M03 | v2 envelope rename 前后与目录 fsync 失败 | 迁移回应未知时不签发；重启只采用完整旧或新 authority，legacy hash 不一致拒绝 |
| M04 | 五类旧 decoder/已支持旧二进制读迁移后的原文件，VoWiFi 两个 owner 各一份 | 全部在物理 dispatch 前拒绝；不能读旁路 legacy 重开写权限 |
| I01 | reserve durable commit 前断连、提交后回应前 kill、GUI 保存返回 ID 前 kill | 原 checkpoint 的同候选 seq 可查询/重取同 identity；不产生第二个 reserve 或物理操作 |
| I02 | 两 GUI 同时读 H、同 seq 不同 nonce/指纹；同 nonce 精确重放 | 只一份 durable slot；另一份冲突且不自动改号；精确重放仅返回同 slot |
| I03 | 旧 reserve 在其 slot 已退役后到达；伪造未来 seq、epoch、store-id；v1 ID 与 v2 字符串碰撞 | 旧 seq 返回410；未来身份拒绝；绝不自动签发或把历史字符串换 namespace 后执行 |
| C01 | retire/GC 与 reserve/claim/finish 并发；跨进程不同 ArchiveStore 实例竞争 | 同一文件锁内重新验证 H/slot；旧请求无“查不到后新增”的窗口；accepted finish 不被 GC 改为 prepared |
| C02 | reserve 后 claim 前、claim 文件 fsync/rename/目录 fsync 任一点失败或 kill | 未确认 claim 不做 I/O；已 durable claim 重启为 unknown；原 ID 永不再次 claim |
| C03 | dispatch 已发生、回执 finish 前 kill；finish durable 后 HTTP 丢回应 | 前者保留 unknown fence，后者回放原结果；不能用新 seq自动补偿物理副作用 |
| R01 | 极小 cache quota 连续完成远超 W 的 v2 操作，期间频繁重启 | 文件条数和字节平台化；最新回执可查；每个已淘汰老 seq 仍永久无 dispatch；H 严格增长 |
| R02 | 结果正文达到单条/总字节边界，finish 持久失败，live slots 全满 | 保留最小安全 owner/fence；新 reserve 拒绝但查询/finish/恢复可用；不删 unknown 腾空间 |
| R03 | eSIM reader/module unknown、SMS accepted 未归档、reader connect unknown、后台 owner 停止未确认 | quota 压力和重启不能解除既有业务阻断；恢复所需的 stop/disconnect/restore lane不被无关未知槽堵死 |
| R04 | public audio start 已退役正文，再发对应 stop；旧 stop 到新 start 后迟到；跨 epoch 引用 | latest generation pin 精确保留；原 stop 不清理后继音频；独立 cleanup identity 不获得 start 派发权 |
| R05 | VoWiFi terminal receipt 压缩、非 terminal 重启为 unknown、观察期限到期、reader/module 混合升级 | 老 request ID 不生成新 session；projection 退役不释放真实 owner；只有原 operation_id/generation 的确定终态才可清理对应 GUI checkpoint |
| G01 | v1 pending checkpoint、未知版本/损坏 checkpoint、v1 ID 查到 unknown/terminal/缺失 | 原字节/ID保留；仅确定原终态清理；缺失不改号。旧 App 在冻结后新造 UUID只能被拒绝 |
| G02 | GUI issuance checkpoint write/readback/fsync 故障、两个 GUI并发、返回410后重启 | 未确认 checkpoint 不发 reserve/dispatch；410不当成功、不换号；公开/internal路径同样受控 |
| E01 | seq/epoch 接近整数上限；删除/损坏 authority、legacy/presence/anchor | checked overflow 拒绝；不回绕，不重建空权威；外部备份回滚只标注无法本地检测的边界 |
| B01 | 全 family 分步升级、一个迁移失败、旧客户端访问混合版本 | capability逐 family准确；失败 family保持拒绝/原语义；没有全局开关允许未迁移账本绕过准入 |

最终证据须保留每个故障点的文件字节/摘要、store-id/epoch/H、dispatch 次数、同源执行命令及输出。S10 完成条件是上述闭环实现与矩阵有可复核结果，而非本设计文档存在。P0–P6 正式验收计分保持原状；本方案不授予任何阶段 PASS，不报告整体完成百分比。
