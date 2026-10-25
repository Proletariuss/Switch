# Switch 2.76：VoCat 参考方案首批执行记录

日期：2026-10-03，Asia/Shanghai。用户已批准推进优化，中间不生成 DMG。对应方案为 [`2026-10-03-vocat-informed-plan.md`](2026-10-03-vocat-informed-plan.md)，本批实施 C1–C5，保持 SwiftUI、Rust 网关/存储与 Go 硬件/IMS 架构。

## 固定基线和范围

- 起点：Switch 2.75/build275 的实际工作区，包含已有未提交与未跟踪源码；Git HEAD 不能单独代表本批基线。
- 私有起点快照：`/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-vocat-p0-baseline/`，3,247 文件、51,268,504 字节，归档 SHA-256 `aaa5642d1ad1127e73340dd8099d3eb02082c6092f7a64888908abf22813c4c2`。目录包含 manifest、文件哈希/权限、Git 状态及工具链。
- 源码仅修改 `/Users/proletarius/Desktop/Switch/src/`，缓存、测试状态和构建放在 Switch 内。未修改原始 Proletarius/Boo/9.29，未访问真实应用数据、凭据、短信或运行日志，未操作读卡器、号卡或生产 App。
- 9eSIM `6982` 与全部 eSIM 下载/认证路径永久排除。未改变短信接收调度、N待语音边界、系统代理默认策略、卡片认证许可或既有通话流程。
- VoCat 仅作设计参考。本批独立实现，无 VoCat 源码、运行时或依赖复制；其根许可证为 Research & Evaluation，不能当作 MIT 项目整体复用。

## 采用的正确性修复

| 候选 | 起点 Fixture | 本批结果 | 取舍 |
| --- | --- | --- | --- |
| C1 历史与会话摘要 | 150 条消息且通知游标已追平，重启展示丢失历史；活跃会话挤掉安静旧会话 | 独立最新历史页、全部会话摘要、按时间和稳定 ID 加载更早消息；摘要总数独立于已加载正文 | 采用。保留 JSON 存储，未引入数据库迁移；不宣称 UI 加速比例 |
| C2 重认证归因 | `ErrReauth` 被记为 tunnel_lost，故障 20→21，恢复等待 42 秒 | 独立 `planned_ike_reauth`，固定现有计划恢复的 2 秒路径、不增长故障退避；处理 IMS teardown 提前到达的竞态 | 采用。没有改为同会话续期，也未宣称实机重连时间为 2 秒 |
| C3 REGISTER 租期 | 本端 3,590 秒＋其他端 60 秒时误取 60；本端 0 秒＋其他端 600 秒时仍成功 | 从实际发送的 Request Contact 匹配本端；本端撤销、歧义、非法值或明确列表无法匹配时拒绝就绪并清理资源 | 采用。缺 Contact 保留弱证据回退，运营商兼容性待实测 |
| C4 Bark 成功确认 | HTTP 2xx 的拒绝、HTML、空/残缺 JSON 等被当成功 | 严格完整服务响应；提交前保存未知标记，响应不确定时暂停，避免自动重发 | 采用。服务接受不等于手机收到，人工重试仍可能重复 |
| C5 Telegram 分片进度 | 三片短信的第二片 503，重启重试共发送 5 次，应为 4 次 | 固定任务分片、内容/版本绑定、逐片保存明确确认；重启只续发未确认片，Fixture 共 4 次 | 采用。增加加密状态大小与逐片同步开销，换取已确认片不重播；不声称 exactly-once |

### C1：展示、通知与转发分别推进

Rust 新增经 App token 认证的 `/api/multistandby/history` 与 `/api/multistandby/conversations`。历史游标绑定 Profile/peer，以 `(created_at_ms, message_id)` 定位，锚点删除后仍可继续。摘要按稳定会话 ID 分页，覆盖长期安静会话；界面按最新时间排序，仅选中会话按需加载正文。

旧 `/messages` 的 revision 分页和 `/changes` 契约继续供通知、转发与旧通知定位使用。展示使用独立内存变更游标，读历史、读摘要不会推进持久通知或转发 checkpoint。旧格式墓碑缺会话 ID 时全摘要对账，失败保留展示游标。完整摘要跨页期间的新会话从首个 revision 边界补齐。

删除 ID 和消息 revision 防止迟到历史恢复已删除内容或以旧 queued 状态覆盖 accepted。即使尚未建立通知基线，选中会话仍更新正文。超过一页的新消息导致最新页不重叠时重锚“更早消息”游标，保留已加载历史并允许补齐中间缺口。N待仍只提供单条消息删除；没有新增批量硬件删除行为。

若已删除消息恰为摘要最新正文，立即使该摘要失效，直到权威对账恢复数量与预览；同步失败或旧完整摘要迟到均不能保留已删正文。不从局部加载数据猜测剩余总数。删除已确认后的摘要失败单独说明“已删除、同步稍后重试”，不把它表述为删除失败。

### C2/C3：就绪和计数以本端证据为准

N待报告分列 IMS 续期、计划 IKE 重认证、意外故障。SWU 可选 `OnReauthStart` 回调在 teardown 前标记本代次计划原因，避免 IMS 提前退出抢占故障归因；取消和旧代次隔离保持有效。未安装回调的既有单线路流程保持原行为。相关本地依赖改动见 [`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md)。旧报告中已经误归因的记录不追溯改写。

租期匹配处理 instance、reg-id 与结构化 SIP URI，保留用户部分大小写，规范域名/IPv6/参数顺序，处理引号分隔及重复参数。其他端的短租期、零值和数值错误不改变本端租期。自己的 Contact expires 优先；缺自己 expires 时使用有效全局 Expires/请求值；响应完全缺 Contact 时继续兼容回退。这种缺证据回退不能证明服务器存在本端绑定，也不是套餐到期检测。

有明确 Contact 列表但无法匹配本端、出现多义匹配或撤销时不再展示就绪，可能暴露某些运营商既有非标准行为；尚未真实注册验证。protected REGISTER 的拒绝 Fixture 检查 retained connection 与 port-s listener 退出。错误码采用固定白名单，不输出 Contact、订阅身份或原始响应。

### C4/C5：可靠转发的确认边界

Bark 接受单个完整 JSON 对象且唯一整数 `code=200`，允许兼容扩展字段并限制响应大小。Telegram 逐片接受唯一布尔 `ok=true` 的完整响应。明确 HTTP/API 拒绝沿用已知失败分类与 Retry-After；网络结果丢失、残缺/歧义响应等为结果未知。诊断输出固定码，不持久保存 URL、密钥、响应正文或原始运输错误。

结果未知和固定 Telegram 分片进度位于既有 AES-GCM 队列。提交前及每次明确确认后落盘，保存失败立即停止后续片和渠道。已全确认但任务完成标记未保存时，可在重启后完成而无需再发 HTTP。旧任务无进度元数据从零开始，不伪造历史确认。消息/分片摘要或版本不一致时暂停，格式更新不重新渲染已接受任务。

目标更换、停用再启用或恢复旧目标都不清除已确认进度和未知状态。UI 展示确认片数并要求用户明确选择未知结果的重试或取消，重试提示可能重复当前片。网络 I/O 不持有队列状态锁，保持原单 worker/in-flight 约束。

本轮验证的是进程重启和加密队列保存边界。既有 `state.save` 忽略目录打开/Sync 错误，本轮未改基础持久化层、未做断电试验，不能声称已验证断电耐久性。远端先接受后丢响应时，人工重试仍可能重复未确认片；服务接受也不能证明 APNs 或最终客户端投递。

## 验证记录

各项失败复现和定向回归：

| 领域 | 日志目录/文件 | 验证范围 |
| --- | --- | --- |
| C1 起点失败 | `build/optimization/2026-10-03-c1-history/baseline-failure.log` | 已追平游标后的历史缺失 |
| C1 改后 | `build/optimization/2026-10-03-vocat-c1-rust.log`、`2026-10-03-vocat-c1-swift.log` | Rust 16 项，Swift 16 PASS 分组；150/250/500 消息、133 会话、相同时间、删除锚点、旧通知及交错响应 |
| C1 摘要删除边界 | `build/optimization/2026-10-03-vocat-c1-deleted-summary-before.log`、`2026-10-03-vocat-c1-deleted-summary-after.log` | 先复现再修复：本地/远端删除成功、摘要失败、旧完整摘要迟到及恢复 |
| C2/C3 | `build/optimization/vocat-c2-c3/` | 原失败；运行时、vendored SWU、imscore 全包 race；计划/故障/取消竞态、租期匹配及拒绝清理 |
| C4 | `build/optimization/2026-10-03-vocat-c4/` | 服务响应、提交边界、重启/配置变更/写盘失败、代理、加密图标、双渠道与队列锁 |
| C5 | `build/optimization/2026-10-03-vocat-c5/` | 分片重启恢复、前后 checkpoint 失败、丢响应、内容冻结、深复制、旧任务与锁隔离 |

交叉复核分三域进行：Swift/Rust 历史与游标、Go N待/IMS 生命周期、Go/Swift 转发队列。Root 复核摘要分页与消息状态交错后补三个 C1 边界；不同实现者发现并复现删除摘要残留，补修失效与成功反馈。C5 由不同实现者复核未发现阻断，明确保留基础层断电耐久性限制。

最终集成验证：

- `scripts/test-functional-optimization.sh`：通过，exit 0，198.877 秒。含转发/IMS/运行时/SWU race、Rust 存储 16 项、十组 Swift Store/策略门禁及真实 Rust supervisor + 硬件禁用的 Go Fixture IPC。跨语言检查共 10 项，Swift N待 16 PASS 分组包含最后的删除成功反馈修复。
- `scripts/build.sh`：最终版本构建通过，exit 0，41.696 秒。包含完整 Swift 类型检查、Rust/Go 嵌入、Release arm64/macOS 27 与签名验证，输出 `/Users/proletarius/Desktop/Switch/dist/Switch.app`，App 2.76/build276、Rust 2.76.0。签名为本机 ad-hoc，未公证。
- 本轮新增 IPC 检查覆盖新展示路由的真实公开 socket 鉴权、Go 入库后 Rust 历史/摘要、重启、删除及展示读取不改变 revision feed。
- 工具链：Go 1.26.3、Rust/Cargo 1.98.0、Swift 6.4、Xcode 27.0（27A266a）。最终清单同时记录机器/工具链，不以版本号代替快照。

聚合日志与机器可读结果保存在 `build/optimization/2026-10-03-vocat-p0-validation/`。前两次完整 Release 发现短信页面长修饰器链类型推断超时；保留 `release-build.log`、`release-build-retry.log` 与各 exit 65 记录。抽取内容、监听与弹窗的 `some View` 编译边界，保持原顺序和状态，无 `AnyView` 或新增包装节点，完整编译随后通过。版本核对发现 Rust 标签尚为 2.75，已同步 11 个 workspace crate 及锁文件，`--locked --offline` 元数据验证通过，最终日志为 `release-build-versioned.log`。最终再次校验签名、Info.plist 与嵌入 Mach-O，记录在 `artifact-verification.json`。

聚合之后仅拆分 SwiftUI 表达式及同步 Rust 版本元数据，业务逻辑未再改；追加完整 Release 和锁定元数据检查，没有重复全套 Fixture。现存音频等 macOS 27 API 废弃警告和重复 libusb 链接警告保留，不在本批扩展改写通话或下载路径。

一次早期重跑误用相对环境脚本路径，source 失败后发生默认 Go 缓存访问；此轮不作为最终验收，未清理可能属于其他工作的系统缓存。后续及最终门禁均从绝对 `SWITCH_SOURCE_ROOT` 加载隔离环境。整个批次未访问真实通知服务、号卡或原应用数据。

## 指标与后续候选

本批采用依据为可复现的正确性缺口，未测 CPU、RSS、端到端通知/短信延迟、真实 UI 绘制或整夜掉线。Fixture 中 5→4 请求是去除确认片重播，不能包装为普遍性能改善。单次构建时间仅是验收耗时，不构成三次冷/热构建收益数据。

沿用原门槛：CPU 相对下降至少 20% 且绝对至少 0.5 个百分点；短信/队列 p95 至少改善 25% 且 50 ms；构建中位数至少改善 20% 且 5 秒，90 天收益至少覆盖成本两倍。没有测量或达不到门槛的性能改造不实施。

| 剩余候选 | 当前状态与触发条件 |
| --- | --- |
| C6 订阅身份/发件账本 | 后续独立批次；先固定设备订阅身份，再处理原子 claim 与不确定发件恢复，不只加前端缓存 |
| C7 UDP 诊断 | 后续独立批次；实测报文往返与超时来源，不能把 socket 创建当成功 |
| C8 安全事件时间线 | 后续独立批次；只记录最小脱敏事件，界定保留量与诊断用途 |
| C9 同会话 IMS 续期 | 暂缓；依赖正确租期与真实自然续期基线，需可关闭试验，不影响现有通话或同卡许可 |
| C10/C11 新功能 | 后置候选：短信离线导出、能力状态、USSD/网络选择/只读 Bot 等按具体使用收益独立选择，不并入本批 |
| C12 刷新/SSE | 暂缓；先测前台、隐藏、最小化各 3×10 分钟及实际 AT/APDU 次数，后台短信/来电/N待检测保留 |
| 泛化重写/数据库迁移 | 停止；未发现需要全面替换架构才能修复的证据 |
| 9eSIM `6982` | 永久排除，不再研究或改动 |

本批最终快照存于 `/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-vocat-2.76-final/`。归档哈希以该目录 `manifest.json` 为准，避免在快照自身记录循环哈希；捕获后校验结果保存在验证目录。起点与改后哈希差异同时记录于 `source-delta.json`，eSIM/下载排除路径保持原哈希。未生成 DMG，未提交/重置/清理 Git，未启动或替换已安装的生产 App。
