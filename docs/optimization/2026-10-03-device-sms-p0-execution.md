# Switch 2.78：设备短信接收确认与发送结果

日期：2026-10-03，Asia/Shanghai。按用户已批准的项目目标继续推进，沿用 [VoCat 参考方案](2026-10-03-vocat-informed-plan.md) C6 设备阶段。该阶段修复已复现的短信确认/清理缺口；完整设备订阅身份与持久发件账本尚未完成。不生成 DMG。

## 基线、测量与范围

先验证 2.77 最终源码快照完整且工作区没有新增/变化，再冻结实际 Switch 2.77/build277 起点。起点目录：`/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-device-sms-p0-baseline/`，3,272 文件、51,432,470 字节，归档 SHA-256 `4a4b733beb096656ca2cbe12ac2b43fe8d38219017483053228a2eef4dab2f90`。当前工作区含历史未提交/未跟踪改动，不能用 Git HEAD 代表该基线。

只修改 `/Users/proletarius/Desktop/Switch/src/`，测试状态、缓存、日志和产物均在 Switch。未访问原始 Proletarius/Boo/9.29、生产用户数据、真实短信/凭据或应用运行日志；未启动/停止生产 App、读卡器、号卡、运营商、真实代理或 Telegram/Bark。9eSIM `6982`、全部 eSIM 下载/认证路径永久排除。保留 SwiftUI、Rust 网关/存储、Go 硬件/IMS 架构、原有语音和 N待 SMS-only 边界。没有复制 VoCat 源码或引入其依赖。

本批采用可复现的正确性修复，不以未测性能立项。保留 CPU 降幅至少相对 20% 且绝对 0.5 个百分点、短信/队列 p95 至少改善 25% 且 50 ms、构建中位数至少改善 20% 且 5 秒以及 90 天收益覆盖两倍成本的门槛。未采集 CPU、RSS、唤醒、UI 帧率、真实网络延迟或长稳样本，不宣称上述性能收益。一次回归/构建耗时仅用于验收记录。

## 采用与停止的取舍

| 问题与证据 | 本批处理 | 成本与剩余限制 |
| --- | --- | --- |
| Modem void 回调不等待归档；坏 PDU/未完成片也会自动删除，纯 AT 队列 fixture 已复现 | checked 回调成功才允许清理；无回调、失败、坏 PDU、未完成片保留 | 新增存储事务锁与复读；没有可信订阅租约的 SM/未知存储保留 |
| 模块 IMS 回调吞掉归档错误；无 archive 也能发布到缓存 | 同一 checked 本机归档入口，失败向 IMS 传播，确认后才 merge | 保留既有 RP-ACK 规则；没有新增订阅字段或当前卡回填 |
| 1000 条保留上限可截掉服务中心时间较早的新短信仍返回成功；目录同步错误被吞 | 所有本次输入须仍在候选档案，8 MiB 写限制，错误恢复缓存，目录确认重试 | 继续现有有限历史；容量失败保留源，不新增数据库/账本 |
| USB 接管使用读取快照后整库清空，可能删除快照后的新到达短信 | 归档、发布顺序修正，取消接管后的自动 SM/ME 整库清空 | 手动清理保留；设备容量要人工管理，增加运维成本换取避免无证据删除 |
| 普通 reader-IMS 可在身份/代次观测变化后仍进入提交 | 同一 session 操作锁内准备前和真正提交前校验 coherent 身份快照 | 仅观测 guard，不是物理卡租约；未观测换卡窗口仍存在 |
| 通道已接受，但归档失败仍回复 `sent:true` | 固定、不可自动重试的 `sms_send_archive_outcome_unknown`；三处设备发送入口保留草稿并查询 | 不等于发件幂等：相同旧 API 请求仍是新操作，设备 ledger 独立实施 |
| 审议草稿跨轮继承分片，可将新第一片与旧第二片拼接 | 单次完整当前存储快照先扫描全组冲突，不跨轮继承 | 保留长短信功能与全部原片；受损历史仅余互补片时来源不能证明 |

完整设备身份预计仍需独立 2–4 人日，持久发件账本 4–7 人日，包含验证/维护；不是交付承诺，也不能扣除此前 N待前端恢复工期冒称已完成。模块 AT/USB 全流程提交租约、可信订阅、历史身份迁移和逐片回执未提供足够边界证据，本批停止扩展。严禁只给 Swift 加 UUID 或把当前 ICCID 填入旧 ME 短信。

## 接收、归档和清理契约

启动注册 `SetCheckedSMSCallback`。新增回调必须仅做归档/发布，不重入 AT 存储操作。旧 void 回调仍可通知，但在事务锁释放后执行且不授权删除，避免旧调用者重入锁。public AT 的 CPMS/CMGR/CMGL/CMGD（含 compound 命令）同样受存储锁保护；无关 AT 与语音队列保持。

接收事务覆盖存储查询/选择、读取、checked 归档、同 PDU 复读、精确索引删除与存储恢复。ME 单片只有持久确认成功、存储复核为 ME、复读 PDU 相同且观测 SIM/RDY epoch 未变时才精确删除。本锁仅协调软件存储命令，不等同于物理卡或模块租约。观察事件可能来不及报告真实热插拔；不承诺实机原子性。

SM/MT/未知存储、解码失败、未完成片和全部长短信片留在源设备。未完成片不发布，也不删除。URC 长短信读取当前完整 CMGL，轮询也使用同一 snapshot 解码；按 storage、观测 epoch、sender、reference 位宽/reference/total 分组。先检查全部分片：同序号出现不同 PDU 时整组拒绝，矛盾索引使整个列表拒绝。没有上轮片缓存，失败后从源重新读取，正文时间取第一片稳定时间。正常完整且无可见冲突的长短信仍发布。

**单次完整快照不能证明所有片的历史归属。** 若人工删除了部分旧片，旧 seq2 与复用同 ref 的新 seq1 恰好互补且没有重复序号，当前协议键不能证明来源；需要可信订阅绑定的持久分片 receipt。该能力不在本批，不宣称全面防错拼或准确恢复受损历史。保留源片便于人工核对，会增加容量压力。手动删除/清空仍由用户原有操作执行。

module/reader IMS 安装回调时固定原 source kind，不按接收时“当前卡”重新标记迟到消息。这里只固定 `dji`/`reader` 设备类别，**不是订阅隔离**；现有设备历史和会话没有获得可信 SIM 身份。checked 错误原样返回给既有 IMS 接收机制，归档未确认不允许 RP-ACK。源码中 API 保持原有 JSON 字段/稳定 ID、通知和转发游标，不迁移历史。

归档继续使用 `.lock`、原子临时文件、file sync、rename、目录 open/sync/close。写前限制 8 MiB；排序/1000 条截断后，本次每个输入 key 都须仍在，否则不写、不发布。保存失败恢复确认前的内存缓存。rename 后目录同步失败时，文件可能已经可见，**不能声称磁盘已回滚**；返回失败、保留源并重试目录确认。重复记录不能跳过该确认。历史列表与原有 1000 条保留策略不是永久接收 receipt 或发件授权账本。

## 提交结果与 reader 观测边界

普通 reader-IMS 在原 `session.imsOpMu` 内，准备前及调用 `Service.SendSMS` 前复用一次 coherent `statusAndUSIMInfo` 快照与既有 call identity matcher。缺身份、断开、reader/SIM/非零 generation 不匹配时拒绝。快照锁在 session 锁前释放，未延长卡认证锁等待网络。multiAuth/N待、module 路径和下载认证代码不改。

这只能拒绝已被 reader manager 观测到的变化；未被观测的物理换卡、重启后 generation 重用不是可信授权证据。设备会话历史继续按设备/对方组织，未实现跨实体卡历史隔离。

设备通道接受后，归档失败返回 HTTP 502、code `sms_send_archive_outcome_unknown`，语义为“发送通道已接受请求，但本机记录未确认。再次发送可能重复，请先核对。”既有错误模型省略 false 的 recoverable 字段，客户端默认 false；没有 `sent:true`。正常成功保留 `sent/segments/device` 原 shape。通道接受不等于收件人已收到。

短信中心、会话回复和旧 Compose 的设备入口共同识别 unknown、保留草稿并使用既有 `refresh()`，不新增 force 扫描，也不自动重发。该刷新通常查询列表/状态；原有首次加载或设备刚就绪时的设备扫描规则仍保留，不能称为所有条件下纯读取。旧 `phone/message/device` 请求和严格 decoder 保持，新 `client_request_id` 尚不被支持。完整防重、断线/重启恢复、AT 多段超时分类与固定 attempt fence 仍需要独立设备 ledger；不能从这次错误文案修复推导它们已经完成。

## 交叉审议、证据与验收

三条审议线覆盖接收/存储、身份/发送及 ledger 边界，分别定向复现后交叉审查。modem 两遍审查修复了新草稿 retained-reference 混拼；Swift 审查避免对未知结果额外强制扫描；root 审查补容量、写上限、失败缓存和目录持久确认。该草稿错误未作为已发布 2.77 的回归描述。

- 起点缺口日志：`build/optimization/2026-10-03-device-receive-before.log`（无 archive 发布、延迟 SMS 被截断仍成功，两项预期失败）；`2026-10-03-device-modem-receive/before.log`（四项预期失败）。
- 身份前后证明与强末门准备中变化 fixture：`2026-10-03-device-identity-boundary/`。全为 synthetic snapshot/codec，无 IMS 网络启动或卡操作。
- 实际旧 handler AST 证明：`2026-10-03-device-ledger-boundary/`，严格 decoder、新字段拒绝、ACK 丢失可重复提交和归档失败错误成功；不能把 baseline proof 的通过称为可靠性通过。USB actual poll 前后 proof 同目录。
- Modem 最终全包 race：`2026-10-03-device-modem-receive/full-package.log`，1.235 秒；root archive/checked receive/send/reader 定向 race：`2026-10-03-device-receive-final/root-target.log`，1.731 秒。
- 新 API policy fixture 与视图 parse：`2026-10-03-device-sms-ui/`。实际一次 HTTP POST、新 502 code、不可恢复、unknown 分类、文案无失败前缀；parse 不代替完整 SwiftUI 类型检查。末轮审查后 fixture 改为 handler 实际固定文案（无“结果待确认”）且省略 false 字段，独立证明 code 后缀识别与客户端默认不可重试；追加 target script 日志位于最终验证目录。
- `scripts/test-functional-optimization.sh` 通过，exit 0，206.847 秒。包括 Go 转发/IMS/Modem/安全事件/运行时/SWU race、设备短信 archive/checked 接收/send/reader guard，Rust 存储 20 项、UDP 19 项、归属表 4 项，十二组 Swift 脚本与 12 项真实 Rust/Go IPC（硬件禁用）检查。聚合后只改测试 fixture 和文档/注释，没有重复已通过的全套门禁。
- `scripts/build.sh` 通过，exit 0，65.075 秒。完整 SwiftUI 类型检查、Go/Rust/libusb 嵌入、Release 构建和签名校验成功。该耗时是单次验收值，不是构建速度收益。
- 追加 `scripts/test-api-request-policy.sh` 通过实际 handler 文案与省略 false 字段 fixture；只有一次 POST，无自动重发。完整 Release 后无业务代码改动。
- 产物 `/Users/proletarius/Desktop/Switch/dist/Switch.app` 已核对 2.78/build278、`com.proletarius.switch`，主程序、两个后端与 libusb 均 arm64，deep/strict 签名校验通过。本机 ad-hoc 签名、未公证。11 个本地 Rust crate 为 2.78.0，锁定离线元数据通过，外部 Cargo 依赖解析不变。Xcodegen 只重生成相同 scheme 内容，已恢复其原文件权限。
- 独立最终只读审查无阻塞；保留已知限制，不将隔离通过等同于实机通过。签名/版本/依赖、范围及快照记录位于验证目录：`/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-device-receive-final/`。
- 与实际起点相比，修改 20 文件、新增 6 文件、删除 0 文件；其中包含版本/生成工程、文档和回归脚本。起点 143 个匹配 eSIM/eUICC/download 的排除文件内容与权限均不变，额外核对 `reader_vowifi_session.go` 只有 incoming callback 接线变化，没有认证/下载改造。

未运行生产 App、真实卡切换、端到端短信/转发、UI 点击、整夜/24 小时观察。没有真实容量/卡片寿命结论。C9 同会话续期和 C12 非关键刷新等待自然续期及资源基线，不因缺少运营商样本扩展重构。不存在自动化后台计划或 DMG。

最终源码快照目标目录：`/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-device-sms-2.78-final/`。哈希以快照 manifest 为准，避免文件记录自身哈希循环。只清理由本批为复现建立、已有完整快照与日志的临时源码展开副本；保留用户源码、正式快照及恢复资料。无 Git 提交/重置/清理，无原始项目变更。
