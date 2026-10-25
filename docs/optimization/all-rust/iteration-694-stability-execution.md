# Iteration 694：回执恢复、卡通道清理和长期资源边界

本批继续执行用户批准的[稳定性目标](stability-goal-2026-10-23.md)，在
`/Users/StatTrak/Desktop/Switch_MAC/src` 的 `3.50/build350` 工作树实施。
主代理负责整合和推演，执行子代理使用 High。没有运行已安装 App、操作真实
Keychain、使用硬件或运营商、发送外部通知，也没有生成 DMG。

以下属于软件定向验证及本批互审，不是全部 P0–P6 验收或最终发布的三轮复核。
原始日志与源文件摘要保存在 `build/reports/stability-694/`。

## 实施范围

- **S05：卡通道清理。** overview、产品元数据、health、rename、switch、delete、
  download preflight/install 均在结果返回前显式关闭逻辑通道。关闭失败优先保留
  不明状态，停止后续 AID 探测。仅明确的不存在 AID 可继续相应探测；rename 的
  只读失败仅在关闭确认后可探测下个 AID，写入错误不得再次尝试。通知打开时
  MANAGE CHANNEL 丢回应或畸形回应也停止，不把可能已分配通道降为普通不存在。
- **S06：Module 原请求核对。** 新增 `POST /api/vowifi/reconcile`；原请求已有回执
  时精确回放。缺失仅在 active singleton、共享准入排他、完整健康耐久账本和
  无 retained owner/task/cleanup 的证明下写入原 ID 的拒绝终态。它不建立会话、
  操作卡片或重新派发。已恢复 unknown 继续围栏。Swift 上传原检查点字节，
  异 ID、非终态或证明不足不清检查点。准入前拒绝也保留耐久回执。
- **S06 兼容互审修正。** `enable` 使用同一有序、大小写折叠、null 与非法类型
  规则解释；不能由普通 Value.get 记录与原 decoder 不同的操作方向。
- **S07：接收期限及配额。** Module VoWiFi、通话控制适配器、备注、串口 actor 接入
  共享绝对 body 期限；仅接收阶段可取消，之后的真实 owner 不受它撤销。增加
  VoWiFi 操作/核对/通话的预算映射。互审发现并修正内部备注 1 MiB、通话投影
  68 KiB 的预留低估，以及 modem close 缺少 control 预留的问题。
- **S08：恢复分类。** 普通启动故障按 typed 网络/代理/REGISTER 结果区分；
  临时故障只在真实清理和租约释放确认后退避重建。401/403/407、配置、模板、
  证书与策略等不变成自动重试。保留既有代理/fake-IP/ePDG 和 EVS/AMR-WB 要求。
  未找到“已确认清理的 N待业务失败被永久缓存”的缺陷；原样提取生产 reaper
  并补 barrier，保留未知清理/panic 的正确围栏。
- **S11：凭据与快照。** 每个规范数据根使用独立 v2 Keychain namespace；真实
  用户目录下固定非阻塞文件锁覆盖所有根的 bootstrap、迁移、stage、active 提交
  和读回。仅默认根可只读迁移 legacy，保留原队列 key。stale GUI 不覆盖新配置。
  当前根保留 active 加两个较低序号完整恢复快照，每事务最多删除八项，硬预算
  16 项；容量未知或持续删除失败时不再 stage，提交后的 GC 失败不撤销有效配置。
  序号溢出、随机源失败均保留原配置并拒绝新增。
- **S11：目录身份。** 凭据提交、配置、测试以及状态、队列、handoff 等 API
  请求前和采用响应前核对原 runtime 根。目录身份核对独立于 Keychain 可用性，
  因而仍允许凭据不可读时查询原测试回执。检查后的任意同用户路径重定向竞态
  不在已证明范围内。
- **S11：辅助正文与 BLE 日志。** 辅助正文缓存限制为 2048 条/8 MiB，深链副本
  为 256 条/1 MiB；正文淘汰不移除 revision/删除围栏，不调用持久删除。
  独立 BLE writer 在运行和排队合计 32 槽内工作，单条含时间戳最多 2 KiB，
  当前和两份归档各最多 64 KiB；写盘、目录与轮转离开 BLE/保活回调。
  队列满、锁忙或磁盘失败只丢诊断。固定路径拒绝链接、非普通文件或权限异常，
  不沿链接写外部目标。已有超限/权限不符旧日志保留并停止该日志写入，不声称
  自动修复它们；部分写入失败可能留下半条诊断。

## 验证与互审

| 软件验证 | 本批结果 | 证据文件 |
| --- | --- | --- |
| eSIM 通道与下载 | 定向 97 passed | `lpa-final.log` |
| 普通恢复意图 / typed 分类 / service tests | 定向 18 / 12 / 35 passed | `integrated-native_vowifi_sessionintenttests.log` 等三份日志 |
| Module 回执 | 首组 23 passed；追加四项已纳入当前完整回归 | `integrated-native_vowifi_sessionmodule.log`、`daemon-full.log` |
| Swift VoWiFi readiness / checkpoint | 完整脚本 exit 0 | `final-vowifi-readiness-swift.log` |
| 转发配置、GC、目录身份 | 完整脚本、19 项 checkpoint、原生产源码 typecheck exit 0；最后复验源摘要一致 | `forwarding-runtime-alias-validation.json`；较早 GC 切面另留 `forwarding-validation.json` |
| 辅助正文与旧格式深链 | 新专项 3 PASS、完整回归 22 PASS，同一生产 Store 二进制 | `s11-body-budgets-final.log`、`s11-multistandby-final.log` |
| BLE 有界日志 | 真实生产 writer 专项 exit 0，含四个真实子进程和 13 组路径/大小负向场景 | `ble-diagnostic-validation.json`、`ble-diagnostic-log.log` |
| 缓存清理工具 | 精确白名单/源码及链接拒绝/保留产物测试通过 | `cache-cleaner-tests.log` |
| Rust 格式 | exit 0 | `rust-fmt-final.log` |
| Rust daemon 完整回归 | **3361 passed / 0 failed / 8 ignored**，运行 1809.11 秒；忽略项是专用受控子进程辅助项，不计通过 | `daemon-full.log` |
| 最后编译确认 | `--no-run` exit 0 | `daemon-final-freshness.log` |

`rust-source-frozen.json` 固定格式化后的 705 个 Rust 源码/manifest 输入，当前核对
无漂移；`swift-and-scripts-final.sha256.json` 固定本批 13 个 Swift/脚本/工程输入。
初次预格式化清单的两个测试/预算文件发生纯格式变化，原始清单保留，不把它
冒充最终切面。完整回归期间与结束后 Rust 输入核对无漂移，包含新增/删除文件
检查；最终清单另留 `rust-source-after.json`。纯格式时间戳触发了最后一次编译确认，
不把编译确认算作额外全量测试。Swift/工程/脚本的变动未经过完整 App 冷构建，
因此 Rust 通过不授予同来源整产品 PASS。

互审发现并修正的新增问题包括：通知打开分配结果未知仍继续探测、rename
只读回退与写入错误混同、Module enable 兼容解析、配额低估及清理容量遗漏、
runtime 目录别名改指、跨根 legacy 序号影响，以及辅助正文淘汰后的旧格式深链。
初次整合编译曾缺少测试用 BodyExt import，随后修正；新增 GET status 测试首次
没有安排模拟 owner 回答 status，导致超时，已补真实 control 回答，未更改生产
status 预算。初次生产 Swift typecheck 使用相对缓存别名触发编译器缓存冲突，
改为规范绝对缓存路径通过，失败日志保留。

缓存互审修正的旧格式兼容回退现有独立回归：2500 条同一会话、单条 revision
缺失的历史中，目标已从两份辅助缓存和当前页面淘汰，再点旧通知仍可从 API
重取。`findMessage` 携带页面观察版本给 resolver 作为 fallback，保持消息自身
revision=nil。较旧页面回应不能覆盖已接受状态，删除后也不复活。

BLE 的跨进程补证使用两个实际并发 writer 及父进程持锁/解锁后的子进程，四个
子进程均 wait/reap。短写与 EINTR 的生产循环已静态审阅，尚未专门注入测试；
不扩写为任意 I/O、断电或恶意同用户竞态验证。

## 缓存处理

清理器现在能单独识别已确认 Cargo 根下的 `debug/incremental`，逐项校验只含
编译对象、缓存和锁文件；保留同根的测试二进制、deps 和发布产物。工作区脚本
预创建 Cargo 根时没有 CACHEDIR.TAG 的情况也有明确测试，不扩大到任意目录。
完整回归和最后编译结束后，确认无构建进程且 lsof 对两个精确路径均无打开文件，
已删除 `build/rust-target/debug/incremental` 与 `build/swift-cache`，回收
11,378,397,184 分配字节，约 **10.597 GiB**。保留 deps、当前测试二进制、源码、
依赖、报告、恢复数据与发行物。见 `cache-open-file-checks.json`、
`cache-removal.log` 与 `cache-cleanup.json`。

## 尚未闭合

- S07 其余收包 collector 与全部入口容量/退出组合，不由本批局部替代。
- S08 N待暂时启动失败、清理后重新注册成功的明确协议级故障注入证据。
- S10 五类操作账本的服务端签发与退役闭环，目前仍是
  [退役设计](stability-ledger-retirement-design.md)，没有删除旧回执腾容量。
- S11 revision/deletion 元数据、用户主动加载的历史和全量摘要仍无 UI 硬上限；
  本批只限制辅助正文，不能推导整个 UI 的 RSS 已有硬上限。
- S12 同步耐久化对协议调度和实时音频的负载测量及必要隔离。
- S13 来源与接口库存人工复核、同源码完整软件门禁、冷构建和 App 启动退出。
  新 Module 路由已有明确人工映射；当前其他指纹和库存仍漂移，不自动批准。
- S14 实际连续 24/72 小时及全部修正完成后的最终三轮独立复核。

正式台账仍为 49 父项、115 叶项、21 场景类，0 PASS / 5 PARTIAL /
110 NOT_REVIEWED；P0 未闭合、P1–P4 验收中、P5/P6 开放。目录计数不是
完整审定的计分分母，不计算整体完成百分比。实机、运营商、真实音频、外部
Telegram/Bark 投递及旧版回退明确未验收。持续目标保持 active。

## 接续批次的源码地图

下一批优先参考 `build/reports/stability-694/s07-remaining-ingress-audit.md/.json`
与 `s12-blocking-map.md/.json`。前者核对 81 处编译 collector，区分 79 处现役
网络接收与两处本地 DTO 重解码；当前统一通话入口仍先经过
`unified_call_control` 的原 2 秒接收边界，不能把后面的共享适配器误报为全部
网络入口都已改造。新确认 USB SIM 三条 RPC 实际 1 MiB 却仅预留 4 KiB、
多条 1 MiB 路由缺配额，以及无 body 期限的入口可占住 normal/control 池和退出
清理闸门。这些是已定位、尚未修复的 S07 项，地图不是验收通过。

后者追实七条异步流程的同步磁盘链，包含 VoWiFi terminal lookup 的全档读取及
目录 sync、公共音频/队列回执、转发队列打开和各投递持久边界、live prepare
配置/IMEI及相邻永久身份账本；同时排除已经 archive_io 隔离的路径。下一批
保持先耐久 claim 后派发、取消不丢 owner、未知结果不重放，不以删除验证或
提前释放来改善延迟。CoreAudio 和两次 Debug 恢复采样仅作待测线索。
