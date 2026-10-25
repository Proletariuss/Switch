# 全 Rust 后端：可执行 API 契约清单（P0）

最新实机验收（Iteration 604）：3.46/build 346 隔离 Rust daemon 连接 ESTKme-RED，确认 SIM World 454/03 并读取10个 Profile；由 Rust 自动代理/路由选择后在约22秒内完成隧道和 IMS 注册。报告没有记录最终传输模式或具体代理出口，不据此声称节点级路由验收。停止、断开和隔离数据清理均获确认。没有切换 Profile、读/发短信或拨号；不据 readiness 标志认定短信/通话成功。机器报告见 [`iteration-604-simworld-vowifi-probe.json`](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-604-simworld-vowifi-probe.json)，详见[工作包记录](iteration-604-simworld-vowifi-registration.md)。

Iteration 604 还复核了 P0 runtime-owner map：179/179 个 Iteration 603 source pins 与当前源码一致，已移除一条关于 serial actor `thread.join` 的陈旧 UNKNOWN；对应全量测试仍是软件证据，不代表真实串口硬件关闭验收，其他 P0 缺口保留。详见[Iteration 604 P0映射复核](iteration-604-simworld-vowifi-registration.md#p0-静态源映射纠错)。

最新发行包仍为：**3.46/build 346**，DMG 校验、只读挂载、版本、deep/strict 签名与包内 Rust daemon 检查通过；本机 ad-hoc 签名，未公证。Iteration 603 的 Rust daemon 全量3036通过、0失败、8忽略及其他静态/离线软件检查仍绑定该工作包；当前实机注册证据只关闭 SIM World 指定条件下的一次IMS注册，不代表完整功能验收。严格 Rust-only 路由/清单门禁为294/294 Rust HTTP owner、0 Go owner；P0台账分母未冻结，P0–P6整体验收未完成，不报告总体百分比。没有生成新 DMG。见[3.46发行记录](../../release-3.46.md)、[Iteration 603](iteration-603-serial-actor-final-join.md)及[Iteration 604](iteration-604-simworld-vowifi-registration.md)。

按用户随后明确要求，已正式生成并验证 **3.44/344 DMG**：GUI与Rust后端实际重编，版本升级后79项定向/辅助回归通过；只读挂载、严格签名、包内实际3.44.0后端17场景/23进程与3种日志退出通过。原完整3036回归仍绑定版本升级前切点，两者之间仅6个版本/工程元数据文件变化。见[3.44发行记录](../../release-3.44.md)。整体P0未闭合，P1–P4验收中、P5完善中、P6未完成，接口294/294；实机入站短信仍暂缓。

当前工作包597：修复短信转发配置取消后的runner/准入任务归属、真实退出结果传播及typed admission游标保护。实际完整daemon3036通过/0失败/8忽略，保留595全部3022个通过名称，新增14项；定向三组30项通过。Release Rust后端、候选包及跨语言持久交接已验证，复用未变581 GUI并核对payload；初始候选未安装、无硬件/SMS/call；后按用户新要求生成正式DMG。40个有限归属/格式/退出family已入源码，343当前节点逐项校验、14历史节点明确superseded或待定位，不计完整图PASS。用户确认2.80及转Rust前归档不再保留，记录不可用且不再反复索要，不据此补验旧版等价性。接口接管100%（294/294，含54HEAD）；P0未闭合，P1–P4验收中、P5完善中、P6未完成，整体N及百分比未冻结。下一包优先处理启动未确认配置高水位及CoreAudio退休缺口。 见[597记录](iteration-597-forwarding-retirement.md)。

历史工作包596：P0入口映射及验收校验工具已写入源码，保留294条API、131个Swift调用点、56条后台能力和115个既有需求叶子；10项工程规则已定义，5项仍待历史支持成员事实。2.63 seed的3087文件已隔离恢复并逐项校验，不替代2.80或真实迁移前基线。本轮仅改资料/工具及过时注释，没有重新运行595的生产回归。接口接管100%（294/294）；P0未闭合，P1–P4验收中、P5完善中、P6未完成，整体分母未冻结；入站短信暂缓，无DMG。见[596记录](iteration-596-p0-scope-and-baselines.md)及[台账说明](acceptance-ledger-2026-10-19.md)。

历史工作包595：实际完整daemon 3022项通过，保留594全部3022个通过名称，新增普通通过0项；8个忽略项为专用进程helper，不计通过。本轮仅在严格typed scan/coverage之后，对真实固定FD的完整raw/stamp/placement进行前后校验并执行record、payload及parent/root同步；读口失败保留Pending，不改写原raw/inode、不重放native。有限I/O错误返回13场景/26子进程通过：1短写控制、7 Pending、5 metadata recovered；B01/B02/B03由真实delegated record/parent/root sync ACK验收。保留原v1 source cut与testartifact，以仅两reviewJSON差异精确绑定最终cut，未重复或改写旧PASS。当前595发布进程退出42场景/84子进程（34定义窗口+8受控负向）和备份105场景/210子进程（27 Pending、5 no-op、10 aborted、63 committed）分别通过；旧594全部416报告独立保留。正常v2/v3 create/import/delete、真实RootProof/库存锁/私有归档和100碰撞规则保持，无 update/replace API。生命周期日志达到上限后拒绝新写，尚未实现日志压缩。三个Release main进程与genuine boundUnknown通过。HTTP固定94为92执行通过+2 year1跳过未验证，原aggregate false/exit1保留，排除于24软件必需门禁。实际清理2项Ruby精确可重建缓存，回收分配空间1792167936字节。接口294/294、100%仅为归属覆盖；P1–P4同步验收、P5完善、P6未完成，无整体百分比。P0候选子证据不是规范台账变更。voice-control-only恢复、deploy/calibration义务、完整P0–P6及真实USB/运营商/长稳仍待验收；用户暂缓入站短信。有限I/O返回错误与进程退出不等于所有OS write/fsync/断电窗口。本轮无硬件/SMS/call、未安装、无DMG。 见[595证据与边界](iteration-595-finite-io-durability.md)。

历史工作包594：当前工作包594：实际完整 daemon 3022 项通过，保留593全部3003个通过名称，新增19项；6个忽略项仅为专用进程 helper，不计通过。现有 native/internal create、public import/delete 已通过真实 RootProof、共享库存锁和独立耐久生命周期提交；删除保留原 inode 的私有归档，v2原始字节及既有碰撞规则保留，无 update/replace API。生命周期日志达到上限后会拒绝新的 create/import/delete 写入；旧 head 与原始备份仍保留可读，尚未实现日志压缩。固定105场景/210子进程软件门禁通过：27 Pending、5 no-op、10 aborted、63 committed；Pending 不等于 physical 恢复成功，也不覆盖所有 OS 写入/fsync/断电故障。当前594发布进程退出42场景/84子进程（34定义窗口与8受控负向）另已通过；历史593的42/84仅独立保留。实际三个 Release main 进程的 fresh import/export/delete/restart 和 genuine bound-main 门禁通过。HTTP 固定94项为92项执行断言通过、2项year1跳过未验证，原 aggregate false/exit1 保留，排除于24项软件必需门禁。清理2项精确可重建缓存，实际回收分配空间1790980096字节。接口294/294、100%仅为归属覆盖；P1–P4同步验收、P5完善、P6未完成，无整体百分比。完整P0–P6和真实USB/运营商/长稳仍待验收；用户暂缓入站短信。本轮无硬件/SMS/call、未安装、无DMG。 见[594证据与边界](iteration-594-backup-lifecycle.md)。

历史工作包593：当前工作包593：实际完整daemon 3003项通过，保留592全部2995个通过名称，新增8项；真实原Owned记录的Running/InitiallyExited清理链、14个断言场景以及34个定义的双目录进程退出窗口和8个受控负向场景通过。这些是inert byte-I/O软件证据，不是实机恢复或断电/任意OS写入与fsync故障验收。HTTP固定94项为92项执行断言通过、2项year1未验证，原aggregate false/exit1保留。现有backup create/import/delete完整生命周期、真实USB/运营商和P0–P6完整验收仍待完成；无update/replace API，本轮无硬件/SMS/call、未安装、无DMG。接口294/294的100%仅为归属覆盖；P1–P4同步验收、P5完善、P6未完成，无整体百分比。 见[593证据与边界](iteration-593-owned-voice-publication-windows.md)。

历史工作包592：完整startup恢复生产链与原独立journal、voice清理、双目录可恢复归档、receipt及同Gate epoch激活已实现；实际完整daemon 2995项通过，保留591全部2964个通过名称，新增31项（完整日志差集）。原final-v1实际2992通过/3失败/2忽略，被拒绝；其Release构建通过但package未验，原始失败和300秒局部12通过/2未确认不改作绿色。本发布仅基于新final-v2真实2995全通过。规定24软件报告与真实bound-main沙箱门禁通过；已执行正向软件链仅为original_voice=None的inert NoLive链，Owned managed voice全执行及两个目录完整crash窗口仍待软件验收；bound-main在拒硬件沙箱下仅Requested1/Pending，不能冒充实机恢复完成。HTTP固定94项中92项执行断言通过、2项year1未验证，整体HTTP与leaf仍false。本轮无实机/SMS/call、未安装、无DMG。接口294/294的100%仅为接口归属；产品P1–P4同步验收、P5完善、P6未完成，无整体百分比。 见[592证据](iteration-592-full-startup-reconciliation.md)及[整体进度](progress-current.md)。

历史工作包591：独立sealed startup-v1只读owner接入真实main；完整daemon实测2964项通过，保留590全部2927个通过名称，新增37项（按完整日志差集）。规定24软件报告及真实bound-unknown journal的main沙箱门禁通过；只读身份/配置policy不等于恢复写入、receipt或Gate解封，USB实机读回仍未验收。HTTP沿用590固定94项：92项执行断言通过、2项year1跳过未验证，整体HTTP及leaf仍未通过。591无实机/运营商测试，589注册仅属历史；用户本轮不测入站短信。接口294/294、100%仅为接口归属；产品P1–P4同步验收、P5完善、P6未完成，整体不计算百分比。未安装、无DMG。 见[591证据](iteration-591-sealed-startup-native-read.md)及[整体进度](progress-current.md)。

历史工作包590：原生AT资源关闭与重连候选失败保留补强；完整daemon实测2927项通过，保留589全部2905个通过名称，新增22项（关闭/重连16项、HTTP包6项，按完整日志差集）。规定软件专项、候选包及退出链通过；HTTP固定94项中92项执行断言通过、2项year1时间被文件系统钳制而跳过未验证，完整HTTP门禁及leaf仍未通过。本轮未运行实机/运营商测试，589注册仅为历史；用户暂缓入站短信。接口294/294仅为归属，整体P1–P4同步验收、P5完善、P6未完成。未安装、无DMG。 见[590证据](iteration-590-checked-close-and-http.md)及[整体进度](progress-current.md)。

历史工作包589：控制证据新增有界滚动归档与实际record锁保护；12项归档和5项真实host控制/子进程/ADB分包组合通过。HTTP转义公共字面段兼容缺口修复，原4项失败保留，同一签名Release产物53项对照通过。完整daemon2905项、新增25项、包内17场景23进程及跨语言/崩溃专项通过。本轮ESTK实机读取1EID/10Profiles，确认SIMWorld454/03；注册及清理通过，用户选择暂不测试入站短信，收信仍未验收。接口294/294仅为接管率；完整HTTP矩阵、physical恢复、实机业务/长稳与P6仍未完成。未安装、无DMG。 见[589证据](iteration-589-control-compaction-and-http.md)及[整体进度](progress-current.md)。

历史工作包579：共享模块准入现可单调撤销新业务，并保留已接纳有限任务的真实清理；语音跨请求保留原ADB计划与待结算义务，未接纳RF任务用类型化回执释放原占用。完整daemon2629项通过（新增51项），API/Go退出、Release后端候选、包内17场景23进程及新增live撤权/正常退出、三种日志退出、10模式向量通过；复用未变的578 GUI。清理约1.37GB可重建缓存。启动恢复权利、legacy并发生命周期、串口/重枚举物理绑定、跨语言落盘及实机业务仍未闭合；安装应用与正式dist不变，无DMG。整体P1–P4同步验收、P5完善、P6未完成；294/294仅接口归属。见[579证据](iteration-579-live-admission-and-settlement.md)及[整体进度](progress-current.md)。

历史工作包578：已打开原生USB AT的语音准备/route现持有同连接Device，并在transition租约安装时原子核对owner、producer、session及代次；修复计数耗尽遗留租约。完整daemon2578项通过（新增23项），API/生产Go退出、Release后端候选、包内17场景23进程、日志退出及10模式向量通过；复用未变的577 GUI。清理约1.35GB编译缓存。无AT/串口、重枚举、共享authority、真实跨语言落盘及实机业务仍未完成；安装应用与正式dist不变，无DMG。整体P1–P4同步验收、P5完善、P6未完成；294/294只表示接口归属。见[578证据](iteration-578-usb-adb-connection-binding.md)及[整体进度](progress-current.md)。

历史工作包577：本轮将一次 ADB 计划固定到原生目标，并冻结备份恢复输入、拒绝路径/内容替换；修复 schema 2 附加 voice 被误恢复的问题。完整 daemon 2555 项通过，生产 Go 退出及 API 门禁、Release 后端候选、包内17场景23进程、三种日志退出与10模式向量通过；本轮复用未改动的576 GUI，没有重新编译Swift。清理两项编译缓存及633个旧Go缓存条目，合计约1.47GB分配空间。共享模块 authority、真实跨语言落盘及实机/运营商验收仍未完成；安装应用与正式dist不变，无DMG。整体P1–P4同步验收、P5完善、P6未完成；294/294只表示接口归属。见[577证据](iteration-577-adb-plan-and-restore-input.md)及[整体进度](progress-current.md)。

历史工作包576：读卡器自动选择统一采用同响应的卡片观测，保留 owner 与独立 BLE 回退已补齐；App 正常退出等待实际后端退出再关闭蓝牙桥，并阻断退出期间重启。43 个 Rust 程序常规3370通过/1失败/1忽略；PAC专项6通过，去重3371项有通过证据，真实跨语言落盘前置仍未通过。完整 Swift reader/menu/退出测试、独立 Release 候选、包内17场景23进程、三种日志管道退出及10模式向量通过。清理7项缓存约2.20GB；安装应用与正式dist不变，无DMG。整体P1–P4同步验收、P5完善、P6未完成；294/294只表示接口归属。见[576证据](iteration-576-discovery-and-app-exit.md)及[整体进度](progress-current.md)。

历史工作包575：读卡器presence/身份围栏和前端操作竞争、短信缓存重试持久化修复已加入；3369个不同Rust用例有通过证据，真实跨语言落盘前置仍未通过。完整Swift readiness及失败关闭对照通过。完整Release候选、签名、包内退出和10模式向量对照通过；清理7项缓存约2.14GB，无DMG。整体P1–P4同步验收、P5完善、P6未完成；294/294只表示接口归属。见[575证据](iteration-575-reader-presence-and-durable-handoff.md)及[整体进度](progress-current.md)。

历史工作包574（2026-10-19）：完整daemon软件测试2518项通过；全工作区3345个不同Rust用例已有通过证据，另1项跨语言前置受隔离偏好设置持久化失败阻挡，未计通过。完整候选、签名、实际daemon退出和最终EVS/WB向量检查通过。ESTK实机连接/刷新后未取得SIM身份，独立系统观测为在位但MUTE不响应，未开始IMS注册；旧安装应用已恢复。清理12项精确缓存约3.78GB，源码和保留产物无漂移，无DMG。见[574完整边界](iteration-574-software-acceptance-and-reader.md)。P1–P4同步验收，P5完善，P6未完成；294/294仅接口归属。

历史工作包（Iteration573，2026-10-19）：daemon数据目录/监听路径互斥与运行中媒体死亡的撤桥/真实退出验证完成；338项定向Rust、16项真实native故障、17场景23进程及完整候选通过。当时全量daemon软件门禁2377通过/141失败；失败保留，后续修复和验收见上方574及[573证据](iteration-573-daemon-and-media-lifetime.md)。

最新源码工作包：[Iteration572原生初始化失败恢复](iteration-572-native-codec-failure-review.md)，150个实际分配失败点、10种模式向量对照及5项真实worker故障通过；普通301项Rust、Swift模拟29项、冻结协议297次和扫描器52项通过。整体阶段与可核实指标见[整体进度表](progress-current.md)；P5完善、P6验收未收口，不将接口100%当作项目100%。native运行期/OS故障、实网业务、长稳及升级回退/性能继续保持未完成。

最新缺口修复与交叉审议（Iterations 564–565，2026-10-18）：备注 writer 保留原始未知 JSON，公开 DTO 不变；有界非阻塞日志及实际 daemon 满/闭管道正常退出测试通过。见 [实现及证据](iteration-564-notes-and-nonblocking-diagnostics.md)。[完整编码重协商审议](iteration-565-p5-renegotiation-residual.md) 明确下一步 prepare/commit/discard、offer/answer 与 RTP 格式过渡要求，尚未实施。整体目标未完成；本轮只构建开发 daemon，未更新完整应用或生成 DMG。

最新软件缺口收口（Iterations 562–563，2026-10-18）：AMR-WB CMR 已穿过生产 IKE/ESP/IMS/RTP/PCM 合成链路，保留模式集合并在原编码状态的帧边界按 period/neighbor 调码率；EVS 优先顺序不变。pending+pristine 的模块启动隔离也补入，并有强制硬件/外连沙箱的耐久性验证工具；该工具仍非生产并发回退方案，旧3.42退出问题保持失败。完整包已重新构建，无DMG。见 [CMR与当前产物](iteration-563-amr-wb-cmr.md)、[pending协议](iteration-562-guarded-legacy-pending.md)。真实运营商通话、短信及自然长稳仍待各自验收。

最新实卡与完整构建（Iterations 556–560，2026-10-18）：补齐 IKE SA_INIT 的 NAT 检测通知后，ESTKme-RED 中的 SIM World 454/03 在原 Surge 香港家宽分流和已验证 ePDG 下，两次隔离候选、一次完整源码构建产物，以及一次私有旧锁触发模块隔离的完整产物测试均达到 IMS `registered`；每次均确认关闭 VoWiFi、断开读卡器、退出测试进程并删除临时数据。完整 macOS 应用构建和严格签名校验通过，包内无 Go 后端，294/294 HTTP 契约由 Rust 负责。合成同目录升级验证中新版本两次正常退出且旧模块锁保持原样，但旧 3.42 回退仍退出 1，完整 P6、通话、短信及长期稳定性未验收；未生成 DMG。见 [实卡及构建证据](iteration-557-simworld-nat-notify-registration.md) 与 [模块隔离边界](iteration-556-module-recovery-quarantine.md)。以下较早条目是当时的历史快照，不代表当前注册状态。

SIM World NAT-T 端口诊断（Iteration 554，2026-10-18）：隔离的 pre-41101 Rust 诊断版在同一卡、同一已验证 ePDG 与 Surge 路径下，经最终 AUTH 验证的回包来源端口和首个 ESP 发送目标均为 4500，IMS 仍未注册、ESP 8 发/0 收。该会话排除了缺少端口跟随这一解释；诊断补丁未合入生产。见 [Iteration 554](iteration-554-simworld-natt-port-diagnostic.md)。

IKE 设备身份兼容（Iteration 553，2026-10-18）：按归档 2.86 Go 字节 fixture，为现有运营商开关与冻结 IMEI 增加仅首次 IKE_AUTH 携带的 3GPP DEVICE_IDENTITY(41101)；缺失、禁用或格式异常时不额外发送，IMS/ESP/代理路径保持原样。离线测试通过，SIM World 实网因果与注册尚待同卡复核；见 [Iteration 553](iteration-553-ike-device-identity-parity.md)。

最新实卡对照与 IMS 配置审阅（Iterations 550–552，2026-10-18）：同一 ESTK 读卡器中的 SIM World 454/03，经同一 Surge 香港家宽节点和同一已知成功的 ePDG 地址，归档 2.86 Go 客户端完成 IMS 注册，当前 Rust 后端在 CHILD SA 后 IMS TCP 建连超时、ESP 8 发/0 收。故当前 Rust 实网注册回归已确认，根因仍待定位；仅放宽不同 UDP 来源端口接收的旧实验未能解决。另按上游 `use_ipsec:false` 与本机 iOS 实现补齐 IMS sec-agree 基础策略，3HK 用户模板报文保持原样。见 [2.86 比较](iteration-550-simworld-286-comparison.md)、[IMS 策略](iteration-551-ims-use-ipsec-policy.md) 和 [同卡实网 A/B](iteration-552-simworld-286-rust-registration-ab.md)。未发送短信、拨号或生成 DMG；此轮不算完成实网/P6 验收。

最新运营商与回退验证（Iterations 546–549，2026-10-18）：上游 `iniwex5/ios-carrier-profiles` 的 `main` 已锁定并核验到 `09b8ae86`，SIM World 仍按 3HK 454/03 选择，3HK VoWiFi/IMS 及本机 iOS 项目协议字段保持一致；旧选择规则由显式兼容层保留。发现 Verizon Ting 新身份变量无法展开后保留其旧配置，未把未验证的新身份写入连接请求。隔离沙箱中完成旧 3.42 与当前 Rust 的合成四数据文件及 SWF1 双向读写/取消验证；另增加 SIM World 所协商 CBC 套件的离线 EAP→CHILD→ESP→TCP SYN 组合测试。见 [运营商刷新](iteration-547-carrier-refresh.md)、[数据回退](iteration-546-sandboxed-data-roundtrip.md)、[SWF1 回退](iteration-549-sandboxed-forwarding-roundtrip.md) 和 [CBC 组合](iteration-548-simworld-cbc-composition.md)。这不证明 SIM World 当前 IMS 注册、实卡短信/通话、已安装应用原位回退或完整 P6 验收；本轮未生成 DMG。

最新回退与运营商复核（Iteration 545，2026-10-18）：隔离启动旧版 3.42 Rust 网关与 Go 子进程，成功只读访问当前 Rust 写出的合成短信归档、N待消息和未知发送记录，四份数据文件未变；旧版在无模块环境退出时仍有自身 ADB 清理错误，完整回退验收未完成。上游最新仍为 `09b8ae86`，SIM World 的 3HK 454/03 选择与 VoWiFi/IMS 参数无新差异，本机 iOS 的 3HK 协议参数与 Switch 一致；见 [Iteration 545](iteration-545-legacy-downgrade-read.md)。无 DMG。

最新实卡 A/B（Iteration 544，2026-10-18）：临时改用未连接 UDP socket 接收同 IP 不同端口回包后，ESTKme-RED 中 SIM World 仍为 IKE/EAP 成功、IMS TCP 超时、ESP 8 发/0 收。实验代码已撤回并重建原 Release 后端；见 [Iteration 544](iteration-544-simworld-udp-port-ab.md)。未改运营商模板、代理规则或生成 DMG。

最新运营商核查（Iteration 543，2026-10-18）：SIM World 按 3HK 454/03 处理；上游最新 454/03 选择器与 Hutchison HK IMS 参数和当前锁定版本一致。修正了模块 454/03 服务网络名称误显示为“电讯盈科”的问题；上游其他大量配置差异需另行完成数据锁定及旧版参数金样复核。见 [Iteration 543](iteration-543-simworld-3hk-carrier-audit.md)。

最新一体化构建（Iteration 537，2026-10-18）：当前 SwiftUI 与 Rust 后端完成 macOS arm64 Release 构建；应用包签名、仅 Rust 后端、动态依赖及包内 daemon 无设备启动/退出通过。见 [Iteration 537](iteration-537-integrated-app-build.md)。尚非干净机器或已安装升级验收，无 DMG。

最新 SIM World 数据面诊断（Iteration 536，2026-10-18）：新增 CHILD SA 后 IKE/保活/无效数据报计数；实卡香港家宽代理路径仍为 ESP 8 发/0 收且无任何 CHILD 后入站数据报。临时精确直连经 Surge 确认为 `DIRECT` 后在 IKE_SA_INIT 阶段超时，规则已撤销。见 [Iteration 536](iteration-536-simworld-post-child-rx.md)。IMS/短信/通话未验收，无 DMG。

最新退出修复（Iteration 535，2026-10-18）：只读通话模式探测留下的空锁文件不再使全新 Rust daemon 在无模块时无限等待 ADB 清理；实进程 SIGTERM/移除 socket 回归通过，旧数据目录仍保守处理。见 [Iteration 535](iteration-535-call-mode-pristine-shutdown.md)。SIM World IMS/ESP 实网缺口仍未关闭，无 DMG。

最新实卡与 SIP 收尾（Iteration 534，2026-10-18）：ESTKme-RED 中启用的 SIM World 仍在隧道建立后 IMS TCP 建连超时，ESP 8 发/0 收；Rust 通话 owner 已为超时或挂断后的迟到刷新 INVITE 最终响应补 ACK，并隔离退役旧通话与新通话。见 [Iteration 534](iteration-534-simworld-and-late-refresh-ack.md)。实网业务未验收，无 DMG。

最新实卡与 SIP 刷新（Iteration 533，2026-10-18）：ESTKme-RED + SIM World 在香港 13 家宽节点仍为隧道建立后 IMS `plain_connect` 超时，ESP 8 发/0 收，业务未就绪；Rust 通话 owner 已补齐无 UPDATE 对端的 re-INVITE 刷新与事务 ACK。见 [Iteration 533](iteration-533-simworld-and-session-refresh.md)。无 DMG。

最新 SIP 主叫事务（Iteration 532，2026-10-18）：初始 INVITE 的 422/Min-SE 现有界重试新事务，旧响应重传只 ACK，逻辑媒体绑定保持；见 [Iteration 532](iteration-532-initial-invite-422.md)。SIM World 实机业务仍未验收，无 DMG。

前一轮 SIP 定时器校正（Iteration 531，2026-10-18）：不含 `Session-Expires` 的成功刷新关闭定时器并保留通话；入站矛盾头部在响铃前拒绝，已接听 INVITE 重传保持完整定时器应答。见 [Iteration 531](iteration-531-rfc4028-timer-correction.md)。

最新实卡诊断（Iteration 530，2026-10-18）：Rust VoWiFi 在会话释放后保留脱敏启动失败诊断；ESTKme-RED + SIM World 复测为隧道建立后 IMS `plain_connect` 超时，ESP 发出 8、收到 0、拒绝 0。见 [Iteration 530](iteration-530-simworld-diagnostic-retention.md)。IMS/短信/通话未验收，无 DMG。

最新 IMS 通话维护（Iteration 529，2026-10-18）：Rust 通话 owner 已实现已协商会话的 UPDATE 定时刷新、Min-SE 重试和到期清理，并覆盖入站刷新；隔离 ESTKme-RED + SIM World 复测在 IMS 注册网络传输阶段失败，实机业务验收仍未完成。见 [Iteration 529](iteration-529-ims-session-timer.md)。无 DMG。

最新 ESP 边界核对（Iteration 528，2026-10-18）：受控 SOCKS5 UDP 4500 往返测试把 AES-256-CBC/HMAC-SHA256-128 的 ESP、SPI/方向、实际 relay 与收包分类连在一起；SIM World 实卡透明代理路径的入站 ESP 仍为零，运营商验收未完成。见 [Iteration 528](iteration-528-cbc-socks-ingress.md)。无 DMG。

最新 SIP 事务核对（Iteration 527，2026-10-18）：被拒绝的同对话 re-INVITE 现在缓存 400/415/488 最终响应，同一事务重传不再变成 500，且原通话保持。见 [Iteration 527](iteration-527-reinvite-retransmission.md)。SIM World ESP 入站问题仍未解决；无 DMG。

最新语音会话核对（Iteration 526，2026-10-18）：Rust owner 对已建立通话的无 SDP re-INVITE 在 200 中提出当前语音编码，ACK 答案验证后才更新 RTP；缺失或不兼容答案结束对话并清理媒体。见 [Iteration 526](iteration-526-offerless-reinvite.md)。SIM World ESP 入站故障仍未解决；无 DMG。

最新语音会话核对（Iteration 525，2026-10-18）：已建立通话的同对话 SDP re-INVITE 由 Rust 通话 owner 应答，媒体目标在 ACK 后更新，并覆盖重传、拒绝及 ACK 超时。见 [Iteration 525](iteration-525-in-dialog-reinvite.md)。SIM World 实卡 IMS 故障仍未解决；无 DMG。

最新离线数据面核对（Iteration 524，2026-10-18）：验证真实 smoltcp 出站 IPv4 TCP SYN 的 IP/TCP 校验和与篡改拒绝，并把 AES-CBC/HMAC-SHA256 CHILD SA 双向键测试扩至 256 位；无法代替代理出口 ESP 收包证据。见 [Iteration 524](iteration-524-inner-ip-checksum.md)。无 DMG。

最新实卡协商核对（Iteration 523，2026-10-18）：ESTKme-RED + SIM World 实际选中 AES-256-CBC/HMAC-SHA256-128，仍在 IMS TCP 建连阶段超时，ESP 提交 8、入站 0；临时诊断已撤回，隔离会话已关闭。见 [Iteration 523](iteration-523-simworld-negotiated-child-sa.md)。无 DMG。

最新协议核对（Iteration 522，2026-10-18）：独立 OpenSSL 合成向量验证 AES-GCM、AES-256-CBC/HMAC-SHA256 ESP 完整线格式，以及初始 CHILD SA 的 PRF+ 密钥方向；未改生产路径，也未验证 SIM World 实际协商套件或代理出口 ESP 回程。见 [Iteration 522](iteration-522-esp-independent-vectors.md)。无 DMG。

最新实卡网络对照（Iteration 521，2026-10-18）：本机与 Surge 均观察到 SIM World 会话切向 ePDG UDP 4500；独立新建 4500 UDP 流仍收到 IKE 回复，但 ESP 发出后无入站 ESP，IMS TCP 继续超时。实验代码已撤回，隔离会话已关闭。见 [Iteration 521](iteration-521-simworld-udp-flow.md)。无 DMG。

最新传输边界（Iteration 520，2026-10-18）：系统代理未开启但 Surge Enhanced Mode 已确认开启时，ePDG 由 Surge DNS 解析且必须经已验证的 `utun`；状态无法读取则拒绝主机 DNS/直连。该组合尚未切换系统代理做实机验收，SIM World IMS 故障未解决。无 DMG。见 [Iteration 520](iteration-520-enhanced-mode-dns-owner.md)。

最新实卡对照（Iteration 519，2026-10-18）：Surge 透明代理回退改用 Surge DNS，并在前置代理不可用时禁止静默主机 DNS/直连。SIM World 在另一 ePDG 地址及香港家宽节点上仍完成 IKE/EAP，但 IMS TCP 对两个 P-CSCF 候选超时，入站分类 ESP 为 0；实网业务仍未验收。无 DMG。见 [Iteration 519](iteration-519-surge-dns-simworld.md)。

最新实卡诊断（Iteration 518，2026-10-18）：隔离 Rust 会话在 ESTKme-RED + SIM World 454/03 上完成 IKE/EAP 和隧道安装，IMS TCP 建连超时。新增有界 ESP 计数后，故障快照为本地提交 8 个、收到分类 ESP 0 个、拒绝 0 个；这尚不能区分代理转发、ePDG 与内层目标。未测试短信或通话，未生成 DMG。见 [Iteration 518](iteration-518-simworld-esp-probe.md)。

最新实机记录（Iteration 517，2026-10-18）：ESTKme-RED + SIM World 454/03 再次完成 IKE/EAP 与隧道安装，IMS TCP 建连仍超时。香港 13 与 15 家宽策略均观察到同类失败；一次保持本地端口的 UDP 发送方式对照也未改善，实验代码已撤回。未验证短信/通话，未生成 DMG。见 [Iteration 517](iteration-517-estk-simworld-retest.md)。

最新源码进展（Iteration 516，2026-10-18）：Rust 语音会话已修正同编码 SDP UPDATE/PRACK 的空体确认与旧 RTP 目标问题；开发版后端及完整工作区 3025/0/1 通过，接口仍为 294/294 Rust。未完成通话会话定时器、完整 re-INVITE、CMR 和运营商实机验收，不能以此标记整体完成。见 [Iteration 516](../../iteration-516-progress.md)。

最新源码契约记录（Iteration 513，2026-10-16）：本轮完成同机隔离源码重建：Rust/Swift/EVS输出全部重新编译，PATH无Go，签名、动态依赖及包内原生启动/退出通过；旧数据和SWF1队列新增3项真实独立进程恢复测试。完整Rust回归3010通过/0失败/1忽略，构建审计最终自检通过，2853个冻结输入零漂移。修复可选源码快照工具残留的Go版本调用。契约294/294 Rust、127个Swift调用点、429个审阅范围和54条HEAD保持；生产业务代码本轮未变。12项设备/业务验收、完整升级回退、受控性能及自然到期长稳仍待完成。隔离重建不等于全新机器或全功能验收，当前整体百分比不作无依据估算。未生成DMG或替换已安装应用。详见[Iteration 513](../../iteration-513-progress.md)。

Iteration480 历史契约结构：347 条 method/path（Rust HTTP owner 300、Go owner 47），127 个 Swift HTTP 调用点，384 个审阅范围，含 51 个 GET 派生 HEAD alias。本轮移除五条 Go CS 私有路由和四条重复公开注册，公开蜂窝 status/dial/answer/hangup/DTMF 保持同路径、由原生 ModuleCallService 接管；新增 IMS history-error 私有路由。完整 native CS service/state/facts/serial/USB scopes 及 Go Manager raw 文件指纹已逐项审阅，两个 Go IMS 事实与错误来源保留有序传递。HEAD 不增加独立 handler 或迁移信用。Go 结构退出仍被六类依赖与 257 个生产文件阻断；源码 census 一致不代表端到端 Go-free 或硬件验收。最新生成数据见 `api-contract-inventory.json`。

**进度审计说明：38.31% 为历史估值，阶段账本未闭合，当前精确百分比待重建。** Rust 证书验证已接入 macOS 系统信任和签名校验；软件验证不等于 Three UK 或其他运营商实网验收。当前剩余验收项以上方 Iteration 513 记录为准。

`api-contract-inventory.json` 是源码提取结果，`api-contract-review.json`
是动态路由和源码审阅记录。校验工具发现清单漂移、未解析调用或不一致的
Rust 路由归属时返回非零；不会启动后端、App、设备或发送短信。

历史源码快照（Iteration 309，2026-10-10）：334个method/path契约，Rust273、Go61（含49个GET派生HEAD alias）；127个Swift HTTP调用点、327个审阅范围。Iteration 305核对了 Go `moduleSIMMu` 的跨业务SIM/APDU事务边界，新增统一Rust物理卡仲裁器工作包；本轮为审计，不改变迁移计分。Rust拥有短信清空/删除/按发送方删除策略、普通USB模块eSIM Profile改名/删除/切换/下载公开路径，以及模块 VoWiFi 操作结果公开 GET/HEAD 与契约校验。普通USB模块下载由Rust运行LPA安装、进度和确认流程；仍依赖认证Go适配器停止Profile切换前的模块VoWiFi，Go operation observer、非USB兼容回退和部分概览缓存也仍在。VoWiFi启停执行和结果快照仍依赖Go。Iteration 210 将 B1 NBLE 专用认证/分帧接入 Swift CoreBluetooth 系统桥，不改变 Rust 迁移分；用户确认参考项目真机验证通过，Switch 仍待实机复验。Iteration 211 新增的 Rust IMS SIP typed-session boundary 尚未接入生产调用路径。Iteration 297 移除Go→Rust短信易变快照发布和Rust内部接收桥；公开短信列表只组合耐久归档与Rust USB预览，Go仅按Rust选定的稳定ID修剪自身重复缓存，Go接收/归档生产者及实体存储工作流仍未迁移。Iteration 298 将租约内BLE AKA APDU移至Rust BLE桥，退役无消费者的Go AKA lease endpoint；Iteration 299 将 readiness 中已迁移的租约内 BLE AKA 桥正确标为具备，但没有开启生产 native session。Iteration 300 修复验证脚本对旧工作区路径的硬编码；不改变产品逻辑。 Iteration 310 将 Rust 原生 AKA/身份 blocking worker 计入 reader reservation admission；Iteration 311 将 worker admission 按 reader 隔离，不同物理 reader 可并行，同一 reader 与 N待 reservation 互斥。未接入 Gate 的直接 Go APDU 消费者仍需迁移，不增加加权进度。 Iteration 309 将 N待 reader reservation acquire 收紧为必需的 reader/generation 身份绑定，并拒绝错误 JSON 类型与超界值；仍不接管 APDU owner 或跨进程仲裁，不增加迁移分。Rust-only结构退出审计仍阻塞于7类构建/运行依赖，包含256个生产Go源文件。Go仍持有跨进程lease lifecycle；keyed lifecycle actor、protected IMS runtime、live SMS/call runtime 均未就绪。计数是源码盘点，不代表端到端 Go-free。

历史源码清单（Iteration323）：336 method/path，Rust276 / Go60，127 Swift调用点，342审阅范围。完整离线门禁、Go并发检测与扫描器43项测试通过；Rust进一步接管reader选择/generation及不可变转换回执，PC/SC以精确物理session/成功关闭证明衔接。Go仍保留物理适配、authMu、native lease/watchdog与在线session，整体加权31.4%不变；此轮源码不在冻结至322的2.97 DMG内。详见[Iteration323](../../iteration-323-progress.md)。

Iteration 178 历史源码清单曾登记 311 个 method/path 契约：245 个由 Rust 处理（含 40 个公开 GET 派生的 HEAD 别名，以及内部 UDS modem actor 的 13 个 POST 动作、IMS SMS transaction actor 的 10 个动作、7 个 module-radio 动作、7 个 call-mode ADB 动作、8 个 reader-pcsc 动作、2 个 reader-hotplug 动作、2 个通用 profile-notes 存储 RPC、2 个 reader-BLE APDU 动作、3 个公开 BLE 操作路由、1 个 VoWiFi 代理检查路由、2 个认证 N待操作认领/状态回调、N待恢复策略 RPC、重连重试 RPC、IMS 注册续期 RPC及两个对应的到期回调），66 个由 Go 处理；扫描报告 127 个 Swift HTTP 调用点和 287 个审阅范围。此为源码路由与调用点清点，不是迁移完成百分比。扣除 HEAD 派生项，独立 method/path 实现为 Rust 203、Go 66；HEAD 沿用对应 GET 的处理归属，不能算成新业务接管。Rust 路由包含聚合、协议诊断及持久化端点，例如 device-snapshot、native-session-plan 和 N待提交仍涉及 Go 业务；公开设备短信发送虽已由 Rust 编排，仍调用 Go 固定硬件路由桥。内部 modem UDS 契约已按具体方法/路径纳入清单，包含 sms-receive 配置/scan 及 initialize 入口；Rust直接处理 CMTI 调度、typed ingest 和六步串口初始化，初始化能力独立协商，就绪前不读删短信。Go仍提供设备采集、scan请求及其他设备生产者；RF策略、偏好唯一writer、启动屏障、业务预约、VoWiFi射频恢复与物理执行已由Rust接管，Go只保留相关业务适配。新的私有call-archive/observe由Rust聚合三来源事实并归档；Go原状态机仍提供观察及有界队列，不代表SIP/媒体已迁移。USB自动接收的inbox/refresh/prune由Rust处理，实际USB AT句柄和SM/ME事务也已由Rust owner提供；Go原始snapshot桥删除，七项私有USB AT动作之外又新增完整device-status采集，固定session/lease内完成14步或RF-off的8步并返回16字段；Go保留显示名/已存号码、配置和radio/eSIM/AKA/语音业务，新增私有storage-operation完整执行USB短信usage/clear/delete/delete_sender，绑定固定session/lease、卡身份和全部PDU来源；新增archive/delete-ids只清冻结ID集合。通话模式新增Rust私有状态分类、运行时缓存与generation分配、ADB部署准备及D4/UAC route生命周期；Rust还通过严格的 `voice_facts`、`usb_interfaces`、`call_interfaces` 阶段持有IMS/VoLTE、USB ADB/UAC及最终call-interface readiness predicates，Go的来电准备、重启确认和备份还原状态消费Rust结果。Go仍保留公开状态/cache、事实采集、USB transition lease及整体恢复编排。GET /api/health现在由Rust生成兼容形状的公开响应和device-snapshot投影，但经认证的私有Go适配器仍提供USB/eSIM观测事实；显式Go兼容模式继续用旧处理器。 Iteration177将受监督串口typed query与USB状态投影的PLMN显示名映射归入Rust共用模块；Iteration178在既有认证modem actor契约内增加device-info snapshot查询，不新增HTTP/UDS method/path，Go继续写设备缓存；Go QMI/MBIM及独立兼容路径仍保留Go映射。
所有 Rust 条目的迁移状态均标为 `rust_handler_present_end_to_end_not_certified`。Iteration 168新增 `POST /api/internal/multistandby/runtime/recovery-plan`；Iteration 169将逐 Profile 故障计数和稳定在线累计时长持久化到Rust runtime journal；Iteration 170由Rust持有启动恢复5秒轮询及Go代次去重；Iteration 171将观察事实归一化与持久报告事件状态推导迁入Rust；Iteration 172迁入维护节拍和短信耐久队列入队；Iteration 173新增逐Profile耐久重试计时、代次栅栏及确定性错峰；Iteration 174新增独立的IMS REGISTER续期deadline持久化与session-generation callback；Iteration 175迁移公开健康响应投影并让device-snapshot复用它，同时由Rust分配并持久化N待重连尝试代次；Iteration 176将公开 `GET /api/status` 响应投影迁入Rust，并复用至device-snapshot；模块AT串口候选发现和有界探测也迁入Rust modem actor；Iteration177迁移受监督串口operator typed query与USB状态投影中的PLMN显示名映射，并通过冻结Go/Rust状态差分。Go仍提供真实Profile session、USB/eSIM健康事实和硬件生命周期，且显式兼容模式保留旧路径。

**历史加权迁移进度（Iteration 314，2026-10-10）：31.4%**，累计14.920/47.5计划人周等值；P2A为4.110/5.5（74.7%），P2B为5.400/8.0（67.5%），P3为1.640/8.0（20.5%），P4为0.750/4.5（16.7%），eSIM/LPA为1.275/5.5（23.18%）。最近计分项仍为Iteration209；Iteration298把租约内BLE AKA APDU移至Rust并退役无消费者Go endpoint，但Go reader lease lifecycle仍在、native IMS readiness仍关闭，因此不增加加权迁移分。Iteration 210 的 B1 Swift BLE 系统桥和Iteration 211 的未接生产 SIP boundary均不计后端迁移分。最近门禁：Rust workspace 1203 passed / 3 ignored；契约清单336条（Rust275/Go61，含49个HEAD alias）、127个Swift调用点、327个审阅范围。Iteration 301–303为EVS/RTP辅助实现和通话状态边界加固，Iteration 304将AT诊断错误映射移交Rust，Iteration 305审计SIM/APDU跨业务事务锁，Iteration 306核对reader lease消费者，Iteration 307将健康检查中两个已确认错误分支的响应映射迁入Rust；Iteration 312–314逐步扩展reader operation admission至托管卡操作、BLE热插拔维护和USIM信息刷新，但Go `authMu`/APDU及reader lifecycle owner仍在，不计加权进度。Rust-only退出检查仍因默认Go fallback和177条Go owner/dependency路由失败；完整Go-free构建/运行、设备与运营商验收仍未完成。Iteration 314复跑完整迁移门禁通过：Rust workspace 1203 passed / 3 ignored；Go vet/全包测试、carrier、USB、radio、HTTP method/HEAD、codec及IMS MESSAGE差分通过。Go仍持有SIM/APDU互斥锁与执行桥。详情见Iteration 189–314进度记录。进度按计划人周估算（误差约±3个百分点），路由数不作为迁移完成率。

Iteration153新增两个私有Rust action：N待`inbound-event`由Rust完成原始事件归一化、profile-scoped网络重试去重、状态报告关联和multipart冲突回退；`reader-device-identity/resolve`在私有token校验后读取或原子创建兼容`imei.json`。Go仍保留IMS socket/event lifecycle和Go-only身份兼容路径，不能据此宣称通话、短信在线收发或VoWiFi全链路已经脱离Go。

Iteration154在既有 `POST /api/internal/ims/sms-transactions` 私有路由中增加 `prepare_delivery_report` action。Rust现在生成入站短信 RP-ACK delivery-report SIP MESSAGE 帧，并与外发 MESSAGE 共用 session CSeq owner；Go仍先发送入站 SIP 200、保持耐久回调顺序、经当前注册流发送帧并派发/匹配最终 SIP 响应。该 action 不增加 method/path 数量，也不表示IMS event/socket lifecycle已迁Rust。

Iteration156在既有认证路由新增`decode_incoming_delivery` action：Rust接管IMS入站RPDU/TPDU解析和`IncomingSMSEvent`字段组装；Go仍负责持久化回调和RP-ACK顺序。同期BLE scan/connect/disconnect公开HTTP处理器迁入Rust，调用Go私有读卡器租约路由协调共享操作，再访问Swift CoreBluetooth桥。Iteration157新增`decode_and_store_incoming_delivery`：Rust一次完成解码和 N 待耐久入库，Go不再序列化入站事件或再调用 store/inbound-event；callback只有收到`store_state`才允许现有RP-ACK流程继续。普通读卡器IMS保持decode-only。SIP session/立即200/注册stream仍由Go处理；无新增method/path，IMS transaction actor action数继续扩展。

## 当前离线工具（Iteration 511 已通过软件验证）

以下为当前源码的工具入口；本轮源码退出和完整软件门禁已通过，实体业务与长期运行仍须另行验收。在 `Switch_MAC/src` 执行，使用 Python 3：

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-rust-migration-contracts.py --diff
PYTHONDONTWRITEBYTECODE=1 python3 scripts/tests/test_rust_migration_contracts.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-rust-migration-contracts.py --require-rust-only
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-rust-backend-exit.py --require-go-free
PYTHONDONTWRITEBYTECODE=1 python3 scripts/tests/test_rust_backend_exit.py
scripts/test-migration-gates.sh
```

契约检查器验证真实源码根、Rust workspace、daemon 与 Swift 目录后，直接检查源码树中的 `.go` 文件及 Go module/workspace 文件；发现残留或重新引入即失败。它不再执行 Go AST 小程序，也不把历史注册清单当作当前 handler。结构退出工具另检查构建、打包、子进程、默认门禁和 CI 依赖。删除尚未完成时，退出检查应报告阻塞；这些静态工具始终不能替代应用隔离运行、数据恢复或硬件验收。

完整迁移门禁依次检查源码/路由与负例、编译原生 EVS archive、执行 Rust workspace 和 Swift BLE 回归，再校验冻结预期文件并运行 Rust 协议示例。输出、编译缓存和自测临时源码由 `scripts/workspace-env.sh` 放入 `Switch_MAC/build`；不在源码目录生成 `__pycache__`。当前工具和构建说明使用 Rust、Python 及必要的原生编译器入口。

## 数据与独立预期

- 运营商源文档：`data/carriers/selectors/*.yaml`、`data/carriers/profiles/*.yaml`；用户窄范围覆盖为 `data/carriers/user_templates/3_hk.json`。
- 运行时 registry 与选择用例：`rust/carrier-registry.json`、`rust/carrier-cases.json`。用 `carrier_export` 从 YAML 重建，不复制旧 JSON。
- 独立投影和参数预期：`rust/testdata/retired-go-oracles/`，来源、SHA-256 和生成记录见该目录的 `direct-manifest.json`、`DIRECT_ORACLES.md`；其余协议文件遵循各自 manifest。
- 原生 EVS 源码：`third_party/evs/`；保留许可位于 `licenses/` 及对应资源目录。资源内历史上游名称不表示启动相应语言的后端。

从源码目录执行以下维护命令，先写入报告目录；路径相对 `Switch_MAC/src`：

```sh
export SWITCH_SOURCE_ROOT="$PWD"
. scripts/workspace-env.sh
mkdir -p "$SWITCH_BUILD_DIR/reports"
cd rust
cargo +1.98.0 run --locked -p proletarius-vowifi --example carrier_export -- --output "$SWITCH_BUILD_DIR/reports/carrier-registry.rebuilt.json"
cargo +1.98.0 run --locked -p proletarius-vowifi --example carrier_export -- --cases --output "$SWITCH_BUILD_DIR/reports/carrier-cases.rebuilt.json"
cargo +1.98.0 test --locked -p proletarius-vowifi --test carrier_export_golden
```

工具支持 `--data-dir` 和 `--revision`。当前默认固定 `09b8ae86a6bb6f2a9bff87e9411e0f67f9c2fb25`，集成兼容层后 golden test 核对 858 个 selector、697 个 profile、2,811 个有序用例及原有拒绝结果，并比较完整语义与确定性。历史 2,726 个旧选择用例另行保留并逐一审计。选择用例由重建后的原生 registry 解析；它是维护工具，不能冒充新的独立 Go oracle。更新数据时须审阅来源锁、完整字段、选择结果和许可，不自动刷新历史独立预期。

## 自动提取与人工范围

- Go 残留：源码树的文件存在性检查；包含测试、vendor、工具和嵌套 module。历史 Go 路由与 AST 行为仅保存在旧记录或注入的负例中。
- Rust：提取 `main.rs` 的 method/path 元组分发和 `ownership.rs` 的显式声明；支持普通与 raw 字符串，排除注释及测试模块。handler/ownership 漏项、生产 API 引用和路径常量变化均需复核。
- 动态 Rust：逐条确认导出 token、消息删除、内部 action 和 reservation 前缀；来源函数和 action whitelist 使用去注释/空白后的指纹，变化后必须人工审阅。
- Swift：枚举固定路径、method 参数、查询串与简单插值；动态 helper、默认 method 和不属于 HTTP 的同名调用显式登记。未解析调用和无法匹配的 method/path 均报错。

Rust/Swift 提取器是针对当前路由形式的受限词法工具，不是完整编译器或类型检查器。新宏、跨文件构造或新的客户端方法必须扩展提取器及负例，不能只刷新 JSON。`go_dependencies` 字段保留历史数据格式；空数组不能替代实际执行、设备与生命周期验收。

## 更新流程

1. 运行 `--diff` 检查新增、删除、改名和消费者变化。
2. 遇到 `MANUAL REVIEW REQUIRED` 时，核对真实分发、路径构造和调用者，再更新动态记录及 `api-contract-review.json` 对应指纹；工具不自动批准语义变化。
3. 新写法补负例；移除路由先核对消费者、旧数据和兼容需求。
4. 用 `--write-inventory` 显式生成清单，随后复跑普通一致性、退出检查与工具自测。指纹不一致、未知调用和 handler/owner 漏项仍须失败。

指纹忽略注释/空白，但定位行号变化也需刷新清单。HTTP 方法、HEAD、405/Allow 等历史 wire 行为由独立冻结预期约束；业务响应、未知结果恢复和实卡行为仍分别验证。下面保留各次迭代的历史账本，其中 Go 路径、旧门禁命令和当时限制仅用于追溯。

## Iteration 089 审阅范围补充

本轮补充模块接收队列、typed ingest、会话栅栏、实际关闭等待和Go UDS适配器的人工审阅。Go `manager.go` 中的反引号raw string超出现有通用scope词法器能力，因此未伪造该文件指纹；其Start/configure/URC归属分流、健康状态和退出接线由人工检查及Go全包race覆盖，不能声称自动scope门禁已经覆盖该文件所有变化。Go AST路由提取与此scope词法限制是两项不同能力。详情见[Iteration 089](../../iteration-089-progress.md)。

## Iteration 090 审阅范围补充

加入initialize私有契约，人工复核8项已有scope并新增9项，合计103项。新增范围包含初始化状态机、Go严格响应验证、open/initialize/close/退出接线；actor全文件指纹用于发现enum/state/独立能力字段的变化，不替代语义或实机验收。manager中的六步调用替换、实际协程退出等待及watchdog仅恢复一次仍按上述限制执行人工审查及race回归。一致性与16项工具自测通过；Rust-only检查仍拒绝默认Go fallback及124条Go归属/依赖路由。详情见[Iteration 090](../../iteration-090-progress.md)。


## Iteration 091 审阅范围补充

新增私有`POST /api/internal/call-archive/observe`，原始事件仍依赖Go设备/IMS生产者；旧ingest保留为兼容入口。新增15个可解析整文件scope，总计118项，覆盖Rust事实聚合与归档、Go有限桥/服务接线及Swift可选错误展示；本轮既有dispatch、生命周期和接线scope同步人工复核。

Go `manager.go` 与 `imscore/call_runtime.go` 的raw string超出现有通用scope词法器能力，两者由人工逐项审阅及全包race覆盖，不伪造自动指纹。Go AST路由提取仍正常；该限制不代表所有观察者行为已由静态工具证明。最终清单一致性及16项工具自测通过；Rust-only按预期拒绝默认Go fallback及125条Go归属/依赖路由，没有scope或清单漂移。完整验证结果见[Iteration 091](../../iteration-091-progress.md)。


## Iteration 092 审阅范围补充

新增Go `POST /api/internal/usb-sms/snapshot`及Rust私有`GET /api/internal/usb-sms/inbox`、`POST /api/internal/usb-sms/refresh`、`POST /api/internal/usb-sms/prune`。原始采集仍依赖Go固定句柄/设备锁，不能以Rust调度入口代替物理owner的退出证明。新增13项可解析scope，共131项；人工复核USB原始有界I/O、producer/session边界、Rust单worker与Inbox分区、按ID清理及清理版本校验、Go公共适配及删除整组SIM锁、各层手动刷新期限和Swift精确白名单。

清单写入/一致性及16项工具自测通过；Rust-only按预期拒绝默认Go fallback及129条Go所有权/依赖路由。最终验证结果与静态扫描范围限制见[Iteration 092](../../iteration-092-progress.md)。Go manager及imscore/call_runtime的既有raw string词法限制仍按人工审查/race登记，不伪造自动指纹；本轮没有以静态清单宣称真实设备验收通过。


## Iteration 093 审阅范围补充

Go原始USB短信snapshot生产接口移除，新增Rust私有USB AT七项POST动作：open/status/acquire/release/close/command/configure。Rust现持有实际AT句柄及本地SM/ME采集；Go仍提供设备/radio/eSIM/AKA/语音业务和配置，七项入口均按现存上游依赖保守登记，不视为端到端Go-free。删除3项随旧Go源码退出的scope，新增9项owner/transport/RPC/事务/语音门禁/ADB适配范围，共137项；全部变化scope人工复核。

清单为210条method/path（Rust110、Go100）和127个Swift调用点；write/check及16项工具自测通过。Rust-only按预期拒绝默认Go fallback和135条Go归属/依赖路由，未降低退出条件。完整验证和限制见[Iteration 093](../../iteration-093-progress.md)。


## Iteration 094 审阅范围补充

新增Rust私有`POST /api/internal/usb-at/device-status`，固定session/SIM lease内完成整个USB设备状态采集和投影。调用仍依赖Go生命周期/偏好/显示名与缓存，按Go依赖保守登记，不将入口改为Rust等同端到端Go-free。人工复核原有5项变化scope，并新增Rust collector、Go状态适配器两个scope，总计139项。

当前211条method/path（Rust111、Go100）和127个Swift调用点。严格请求字段、I/O期限、实际worker持有、VoWiFi inhibit、旧session隔离、取消准入与持lease发布缓存均已交叉审阅；85组Go独立投影→Rust真实collector差分加入永久门禁。最后清单一致性、工具自测及Rust-only负向门禁结果见[Iteration 094](../../iteration-094-progress.md)，不以静态清单代替硬件验收。


## Iteration 095 审阅范围补充

新增私有 `POST /api/internal/usb-sms/storage-operation` 与 `POST /api/internal/sms/archive/delete-ids`。前者由 Rust 执行完整物理存储计划和来源证明协调，仍依赖 Go 公开适配、模式准入与固定 lease；后者仅执行冻结 ID 集合的单次耐久历史清理。单条/发送方/清空不再通过旧 Go AT 多命令计划，不能据此宣称 Go 公开业务已退出。

新增 8 项完整文件 scope，复核 11 项已有变化范围，当前共 147 项。复核覆盖固定卡/session/PDU证明、部分确认和未知结果、选择与恢复、生命周期锁与实际关闭等待、隐藏发送方、旧结果不清新会话、精确本地历史同步及各层方法/路径期限。清单共 213 条 method/path（Rust 113、Go 100）、127 个 Swift HTTP 调用点；写入/一致性检查与 16 项工具自测通过。Rust-only 按预期拒绝默认 Go fallback 及 137 条 Go 归属/依赖路由，详细日志与限制见 [Iteration 095](../../iteration-095-progress.md)。


## Iteration 096 审阅范围补充

公开 GET/POST `/api/module/flight-mode` 从 Go 转交 Rust，并新增 7 个私有 POST `/api/internal/module-radio/` 动作：configure/query/bootstrap/refresh/reservation/vowifi/action。Rust持有偏好、策略、预约/intent和物理计划，Go调用者仍在生产；新增条目按实际业务依赖登记，不能据此宣称Go已退出。

删除旧 Go policy 文件对应的 1 项 scope，新增 16 项，当前共 162 项；复核覆盖实际USB/serial会话、原子抑制门禁、producer撤销、未知结果和token/sequence栅栏、同会话启动恢复、通话协调以及Swift精确请求期限。当前清单为 220 条 method/path（Rust 122、Go 98）、127 个 Swift HTTP 调用点。最终写入/一致性检查及 16 项工具自测通过；Rust-only 按预期拒绝默认 Go fallback 及 144 条 Go 归属/依赖路由。flight-mode 本轮仅验证 GET/POST；旧 GET 隐式 HEAD 在新入口仍落 Go fallback，属于待补的 P0 方法兼容边界，不能宣称全部旧 HTTP 方法等价。完整实现和验收证据见 [Iteration 096](../../iteration-096-progress.md)。


## Iteration 097 审阅范围补充

公开 `public_http.rs` 的57条路径方法表由独立抽取的Rust handler及同path Go注册共同校验：漏项、多声明、重复、非法方法/模板、HEAD无GET、内部路径和缺整文件指纹均拒绝。20个HEAD条目明确标为GET派生alias并继承owner及Go依赖；它们不是新handler或额外迁移分。旧Go派生21条路径保留405/Allow；纯native错误和Go专属方法仍沿原路径。

外部测试源码现在依据真实 `#[cfg(test)] mod` / literal `include!` 引用图识别，不按文件名猜测；共享生产引用仍保留，复杂未知条件/路径拒绝审阅。当前排除15项只被测试引用的源，包括两个新增HTTP测试模块。复核5项已有变化scope并新增`handle_request_inner`及`public_http.rs`整体范围，合计164项；两项已有测试声明的hash变化经旧提取规则复算，确认未改变其生产代码。

当前清单240项（Rust142、Go98），含20项HEAD alias，Swift127调用点。实现、209组差分和验收限制见[Iteration097](../../iteration-097-progress.md)；URL清理/转义/尾斜线仍未验收。

Iteration097 最终 write/check 及 41 项工具自测通过；Rust-only 仅因默认 Go fallback 和 151 条 Go 归属/依赖契约而按预期失败，较上轮增加的 7 项来自 HEAD 继承原 GET 依赖。

## Iteration 098 审阅范围补充

新增 8 个私有 `POST /api/internal/reader-pcsc/` 动作：open、status、acquire、renew、release、transmit、reset、close。它们有明确的方法、动作白名单、严格请求字段和 160 KiB 请求上限；停止期间仅接受 status/release/close 清理。Rust 持有真实 PC/SC 句柄、完整系统事务、APDU 执行、租约到期和复位/关闭确认，Go 与原生 LPA、身份读取、AKA 共用固定 capability。下载预检和安装、通知 AID 遍历、身份核对和 AUTH 分别保持在完整事务内。

Go 仍提供读卡器选择、业务 generation、准入、缓存、切卡收尾及 BLE 兼容。原生租约回调使用限定生命周期的 Context capability，保留 authMu，不再通过全局借用开关放行其它 goroutine；复位仍遵循 ESTK 专属规则。未知结果不会触发 APDU 重放，关闭未确认会保留实体资源责任；旧 producer、旧 session 和延迟关闭不能覆盖新会话。公开读卡器不存在错误保持 `409 / reader_unavailable`，私有 owner 的错误码和状态仍独立。

新增 21 项 owner、系统桥和 Go/Rust 消费者 scope，复核 20 项已有变化范围，当前共 185 项；原生 owner 使用完整生产文件指纹，包含 session 发布、close tombstone 与 producer 的同锁核验。清单共 248 条 method/path（Rust 150、Go 98），其中 20 条为 HEAD alias；Swift HTTP 调用点仍为 127 个。8 个新私有动作全部保留 Go 业务调用者依赖，不能据此宣称端到端 Go-free。

最终 write/check 均通过，41 项工具自测通过；Rust-only 仅因默认 Go fallback 和 159 条 Go 归属/依赖契约而按预期失败，没有指纹或清单漂移。新增 2 个显式测试模块后，共排除 17 个仅测试 Rust 源文件。离线 fake-I/O、错误分类和取消/关闭竞态验证不等同于实体卡复位、eSIM 写卡、IMS 长时在线或最终 Go 退出验收。契约工具运行结果见 `Switch/build/optimization/2026-10-06-pcsc-contract-*.log`；源码清单一致性与完整应用构建各自提供独立证据。

## 2026-10-07 审阅范围补充

新增 Go 私有 `POST /api/internal/reader/pcsc-presence-lost`：Rust 对空闲且没有租约的 PC/SC 会话，在两次成功扫描均确认缺失后，按原 producer、reader、session 与 sequence 栅栏关闭实体句柄。实体关闭确认后才向 Go 发送同一身份的业务清理请求；Go 需要确认旧 VoWiFi 与 N 待 runtime/owner 已退出才回执。超时、扫描失败、实体关闭失败或业务清理失败不会放行新会话。Go 仍负责读卡器选择、业务代次、VoWiFi/N 待收尾和 BLE 监测；离线测试不能证明真实拔卡时系统调用一定完成。新 Go 路由是未迁移依赖，不增加 Rust 接管数。

设备短信转发生产者改为读取 Rust 的 `/api/sms` 同源持久归档与 Go/USB 临时收件分区，不再从 Go 的公开 `/api/sms` 拉取。Go 临时快照携带子进程 producer 身份；旧子进程退出后 Rust 原子轮换身份。转发入队与账本落盘在同一身份锁下复核，防止旧代请求跨异步等待在退出后提交。短信硬件采集仍有 Go 依赖，此处只收紧数据源与提交边界。

同时审阅了公开路径规范化及通话/短信客户端模型变化。Go ServeMux 离线抽样核对了重复斜杠、`.`/`..`、查询串、HEAD、CONNECT 和转义 `%2e` 的 307/Location/响应体行为；这不是任意 URL 的完整等价证明。新增 PC/SC presence、Go handoff、BLE 监测、N 待收尾、短信转发 producer 与提交 guard 等整文件指纹，当前共有 **191** 项审阅范围。

最终 `--write-inventory`、普通一致性检查及 **41** 项工具自测均通过。清单为 **249** 条 method/path（Rust **150**、Go **99**，含 **20** 个 HEAD 别名）与 **127** 个 Swift HTTP 调用点。`--require-rust-only` 按预期只因默认 Go fallback 和 **160** 条 Go 归属/依赖路由失败，没有额外的源码指纹、路由或调用点漂移。日志位于 `Switch/build/optimization/p0-contract-{write,check,selftest,rust-only}-2026-10-07.log`。该门禁仅覆盖路由层；完整无 Go 构建、系统桥行为、实卡热拔插、长时 IMS/VoWiFi 与数据升级仍须分别验收。

## Iteration 102 审阅范围补充

Rust 新增公开 `GET/PUT /api/esim/notes` 与私有 `POST /api/internal/profile-notes/phone`、`POST /api/internal/profile-notes/remember-phone`。Rust 统一持有旧 `profile-notes.json` 数据格式、读取/原子写入和号码回填；Go 删除公开读写及本地文件 writer，只通过认证 UDS RPC 暂时调用号码 lookup/remember。两条公开路由从 Go 转由 Rust，另有公开 GET 的 HEAD 派生项及两个 Rust 私有兼容调用。持久化保留既有字段格式，限制文件/请求大小、原子替换、权限、符号链接与损坏文件处理；该范围不等同于 eSIM LPA 或其他 eSIM 业务已迁移。

最终清单为 **252** 条 method/path（Rust **155**、Go **97**，含 **21** 个 HEAD 别名）、**194** 个审阅范围及 **127** 个 Swift HTTP 调用点；普通一致性检查与 **41** 项检查器自测通过。`--require-rust-only` 按预期因默认 Go fallback 和 **158** 条仍有 Go 所有权/依赖的路由失败。完整离线回归、Go 兼容调用边界与 Release App 构建见 [Iteration 102](../../iteration-102-progress.md)。这两个持久化 API 的所有权切换不把整个 eSIM 工作包计为完成；仍按粗粒度 24.9%（±3 个百分点）报告，不从路由数量推导进度。

## Iteration 106 审阅范围补充

`GET /api/network/services` 由 Rust 调用系统 `networksetup` 并负责解析、旧 JSON 投影与错误响应；Go 原 GET handler 已移除。为保持 `module` 标记，Rust 仍读取 Go `GET /api/health` 中的 USB 产品名；网络顺序变更 `PUT /api/network/services-order` 继续由 Go 所有。HEAD 是 Rust GET 的派生别名，继承上述 Go 依赖边界。

刷新来源指纹后，契约清单为 **256** 条（Rust **166**、Go **90**，含 **25** 个 HEAD 别名）、**199** 个审阅范围；扫描报告 **127** 个 Swift HTTP 调用点。普通检查和 **41** 项工具自测通过。Rust-only 门禁仍因默认 Go fallback 及 Go owner/dependency 路由而失败。route census 只衡量入口归属，不能替代 Go-free 构建、系统桥行为或硬件验收。呼入按钮布局修复已在 [Iteration 106](../../iteration-106-progress.md) 记录，不计为 Rust 迁移进度。


## Iteration 107 审阅范围补充

`GET /api/network/traffic` 的只读 `networksetup`、`ifconfig` 与 `netstat` 调用、模块网卡选择、计数解析、旧响应投影和会话基线现由 Rust 执行；Go 原采样 handler 与基线已删除。Rust 仍从 Go `/api/health` 读取 USB 产品名，网络服务顺序 PUT 仍由 Go 执行；派生 HEAD 继承同一依赖。命令有时限和输出上限，不改变网络配置、不操作 SIM。

刷新审计指纹后，契约清单为 **257** 条（Rust **168**、Go **89**，含 **26** 个 HEAD 别名）、**200** 个审阅范围；扫描报告 **127** 个 Swift HTTP 调用点。Rust daemon **704 项通过、3 项忽略**，检查器 **41** 项自测通过，普通契约扫描通过。整体计划完成估计仍显示 **25.1%**（11.945/47.5人周，四舍五入后百分比未变）；本地没有进行网络/实体设备测试，也没有生成 DMG。

## Iteration 108 审阅范围补充

`POST /api/network/check-4g` 现由 Rust 在本机认证分发中直接处理，使用有界 `/sbin/route` 调用和共享的 ifconfig 解析；Go 原 handler 已移除。它没有 Go health 数据依赖，但更广泛的 AT 网络诊断和服务顺序写入仍由 Go 执行。原有根据活动非 `en0` 以太网接口推断 4G 的逻辑被保留，没有声称已识别具体模块服务。

契约清单为 **257** 条（Rust **169**、Go **88**，含 **26** 个 HEAD 别名）、**201** 个审阅范围和 **127** 个 Swift HTTP 调用点；普通扫描和41项工具自测通过。整体进度为 **25.2%**（11.970/47.5人周）。Release App **2.86/build 286** arm64 构建与严格签名验证通过；无 DMG。


## Iteration 109 审阅范围补充

`PUT /api/network/services-order` 的 Rust handler复用受限 networksetup清单，验证至少两个名称、空值/未知服务和重复项，再经POSIX shell安全转义与Base64构造管理员授权的 `osascript` 请求。Go原handler、请求校验和命令构造均已移除。方法策略维持该路径仅PUT，其他方法仍回报405/Allow；请求体采用1 MiB上限、总执行期限3分钟，命令标准输出和错误输出分别限1 MiB。

完整 `proletariusd` 测试 **712通过、3忽略**；Go主包测试、`cargo fmt --check`、`git diff --check` 与41项契约扫描自测通过。刷新后的契约清单为 **257** 条（Rust **170** / Go **87**，含 **26** 个HEAD别名）、**201** 个审阅范围和127个Swift HTTP调用点。正常契约扫描通过；`--require-rust-only`仍因Go fallback和其他Go归属/依赖而不能通过。测试没有运行 `osascript`，不会改动实际网络服务顺序。总计划进度更新为 **12.010/47.5≈25.3%**，P2A为约**63.64%**。

## Iteration 110 审阅范围补充

Rust 接管遗留 GET /api/voice/enabled 与已停用 POST /api/voice/enable。前者调用 Rust modem actor 的固定 USBCFG 查询，并兼容读取/更新 voice-enabled.json；查询失败时返回文件中的旧状态。后者保留限长 JSON 校验和原 409 停用提示，不改变模块配置。Go handler、启动缓存与文件writer已删除。

Rust daemon **719通过、3忽略**，Go主包测试、Rustfmt、41项契约工具自测和普通扫描通过。清单为 **258** 条（Rust **173** / Go **85**，含 **27** 个HEAD别名）、**202** 个审阅范围、127个Swift HTTP调用点；UAC只通过离线伪终端验证，尚无实体模块测试。P2A增加0.025人周至64.09%，总进度为 **12.035/47.5≈25.3%**；不生成DMG。


## Iteration 112 审阅范围补充

`GET /api/call-mode/backups` 由 Rust 直接认证处理，读取既有 module-backups 目录并投影 schema 0/2/3；过滤非法标识、符号链接、非普通/空文件及超过64KiB的备份，格式损坏的文件保留为不可恢复条目。Go 删除公开列表 handler；备份创建、导入、删除、导出及硬件恢复仍由 Go 持有，不能把该路由描述为整个备份功能 Rust 化。

Rust daemon 725通过、3忽略；Go主包测试、Rustfmt、41项合同工具测试和正常扫描通过。清单为259条（Rust175 / Go84，含28个HEAD别名）、203个审阅范围和127个Swift HTTP调用点。损坏JSON的报错文本未证明与Go逐字一致；没有访问读卡器或实体卡。Release `Switch.app` 2.86/build 286 arm64构建与严格签名验证通过。P2A增加0.025人周至约64.55%，总体为12.060/47.5≈25.4%；不生成DMG。


## Iteration 113 审阅范围补充

`GET /api/call-mode/backups/export` 的原始文件下载现在由 Rust 直接认证处理。ID 仍受既有文件名模式限制；读取使用 `O_NOFOLLOW`、普通文件检查和 64 KiB 上限，返回原始 JSON 字节及附件/Nosniff/Last-Modified headers。Rust保持条件GET、单段和多段字节Range/416语义；HEAD沿用公开GET别名。Go 的导出API handler与对应Go HTTP测试删除；备份写入、导入、删除和硬件还原仍属于Go。

Rust daemon 728通过、3忽略；备份专项9项通过；Go主包测试、Rustfmt、41项合同工具自测和普通扫描通过。清单为260条（Rust177 / Go83，含29个HEAD别名）、203个审阅范围和127个Swift HTTP调用点。Release `Switch.app` 2.86/build 286 arm64构建成功并通过严格签名验证；没有访问读卡器/实体卡或生成DMG。P2A增加0.025人周至3.575/5.5（65.00%），总体为12.085/47.5≈25.4%。


## Iteration 114 审阅范围补充

公开 `POST /api/call-mode/backups/import` 从 Go 转由 Rust 负责。该操作先限长并校验备份schema、USB配置和还原命令，再将上传的原始JSON以私有权限写入唯一命名文件，并在返回摘要前重读校验。`null` schema维持Go解码为零值的兼容语义；畸形JSON的解析诊断文案未做逐字兼容承诺。Go仍创建备份、删除备份并执行硬件恢复。该endpoint属于原有Go method/path的所有权转移，契约总数不因迁移增加。

新增Rust测试覆盖原字节保存、权限、唯一名称碰撞、旧schema、校验失败、空body与超限body；Go重复导入实现/测试删除。合同清单保持 **260** 条，owner更新为Rust **178**、Go **82**，HEAD别名29、Swift调用点127。daemon **731通过、3忽略**，Go主包测试与通话接听/拒接回归通过；完整应用2.86/build286 arm64构建及严格签名验证通过。P2A增加0.025人周至**3.600/5.5（65.45%）**，总体进度为**12.110/47.5≈25.5%**。


## Iteration 115 审阅范围补充

确认删除备份接口 `POST /api/call-mode/backups/delete` 现由 Rust 认证处理，Go route 与 handler 已删除。Rust 保留确认字段、1 MiB 请求上限、ID trim/白名单、先读文件再检查占用的顺序、400/404/409/500 状态及成功 JSON。文件校验沿用同一 no-follow reader，拒绝符号链接、非普通文件、空文件及超过64KiB的备份。

删除期间，Rust module-radio service 设置一个同锁互斥标志，禁止 Go 再取得 `call_mode` reservation。为覆盖 Rust daemon 在 Go 硬件操作中途重启的情况，Go 操作在取得 Rust reservation 后还会持有 data root 下的 mode-0600 `call-mode-operation.lock`；Rust 删除必须非阻塞取得同一系统 flock，冲突返回既有409并保留文件。该协调文件不含业务数据。Go 仍创建备份并执行硬件恢复；本项只接管文件删除，不代表通话模式整体迁移。

Rust daemon **734通过、3忽略**，删除接口/互斥专项、Go主包 race、`go vet`及共享锁文件独占/释放/符号链接负例通过。迁移工具41项自测、普通契约检查、Rustfmt与`git diff --check`通过。契约清单为260条（Rust179 / Go81，含29个HEAD别名）、203个审阅范围、127个Swift HTTP调用点。Rust-only门禁按预期失败：默认Go fallback仍在，148条路由仍有Go owner/dependency。Release `scripts/build.sh` 构建2.86/build286 arm64 `dist/Switch.app`，严格签名校验通过。没有连接设备、操作实体卡或生成DMG。P2A计入+0.025计划人周至**3.625/5.5（65.91%）**；总进度为**12.135/47.5≈25.5%（±3个百分点）**。完整 Go-free build、模块硬件业务、IMS/VoWiFi与实机验收仍未完成。


## Iteration 116：安全诊断事件与呼入按钮布局

Rust 维护诊断事件的闭合字段模型、序号与256条环形历史。公开 `GET /api/diagnostics/events` 由 Rust 认证处理，兼容 HEAD 与旧方法错误；私有 `POST /api/internal/diagnostics/events/append` 仅允许内部 UDS token 调用，并拒绝未列入 DTO 的字段、原始错误正文及超限请求。Go 事件生产者只向512项有界异步队列提交脱敏枚举数据，后台单worker写入 Rust；队列满时丢弃最旧待处理事件，不影响业务路径。

同时，通话页呼入操作从自适应网格调整为整组居中的水平按钮行，解决接听和挂断按钮相对卡片偏左。Rust daemon **739通过、3忽略**；Go race、`go vet`、15项跨语言事件集成、41项合同工具测试、普通契约扫描、Rustfmt、差异检查及 `Switch.app` 2.86/build286 arm64 签名验证通过。最新清单262条（Rust182 / Go80，含30个HEAD别名）、208个审阅范围、127个Swift调用点。Rust-only门禁仍有147条路由依赖Go所有权或实现；没有实机/实体卡验收，也没有生成DMG。


## Iteration 117：优化契约扫描器词法复杂度

Rust 词法扫描器原先在每个 token 处对剩余源码切片并执行正则，完整 daemon 源码扫描出现持续满核耗时。现使用预编译 regex 在原始字符串偏移处匹配，避免构造反复扩大的后缀副本；20,000行 raw-string 夹具验证字符串/comment边界保持不变。合同工具测试42项通过；完整清单检查4.88秒通过，262条method/path（Rust182 / Go80，含30个HEAD别名）、208个审阅范围和127个Swift调用点均与登记一致。Rust-only门禁仍未完成。


## Iteration 119 审阅范围补充

私有 `POST /api/internal/call-mode/backups/restore-data` 由 Rust 负责按 allowlisted ID 无符号链接读取、限长与 schema/还原命令校验，并仅返回已确认可还原的 DTO。Go 的 API 与后台恢复 worker 不再直接打开或解析备份文件；硬件状态机、当前模块身份/USB匹配和 AT 写入回读仍由 Go 执行。endpoint 需要内部 token，daemon 停止期间拒绝还原读取。

Rust daemon **742通过、3忽略**；Go call-mode backup/operation 定向 race、契约工具42项自测、普通全量扫描、Rustfmt、`git diff --check` 通过。清单264条（Rust184 / Go80，含30个HEAD别名）、210个审阅范围、127个Swift HTTP调用点。Release `Switch.app` 2.86/build286 arm64构建与严格签名通过；没有实机/实体卡操作或生成DMG。

P2A计入 **+0.025计划人周**，由3.685提高到**3.710/5.5（67.45%）**；整体进度由12.220提高到**12.245/47.5≈25.8%（±3个百分点）**。Go硬件状态机、IMS/VoWiFi与最终零Go验收仍未完成。详情见[Iteration 119](../../iteration-119-progress.md)。


## Iteration 120：删除 Go 备份存储旁路

删除 Go 中未注册的 call-mode 备份列表、文件读取、解析和摘要投影实现，消除 Rust 已接管后的第二套文件 owner。Go 保留硬件状态机、安全 DTO 检查与 Rust 私有 UDS 客户端；公开路径和数据格式未变。Go 全包 race/`go vet`、Rust 备份专项17项、合同工具42项、全量清单、Rustfmt、差异检查及 Release `Switch.app` 签名验证通过。清单仍为264条契约（Rust184、Go80，含30个HEAD别名）、210个审阅范围、127个Swift调用点。没有实体卡操作、真实通话或DMG。

此项计入P6 Go退出准备 **+0.025计划人周**，总进度为**12.270/47.5≈25.8%（±3个百分点）**；P2A保持**67.45%**。Go模块硬件状态机、IMS/VoWiFi及零Go终态尚未完成。详情见[Iteration 120](../../iteration-120-progress.md)。


## Iteration 121：备份格式校验收归 Rust

Rust 备份 owner 已校验创建与恢复输入的 schema、时间戳、模块 IMEI、USB 功能位、还原命令和 IMS/VoLTE 字段。本轮移除 Go 对同一备份 DTO 的重复验证及重复测试；备份创建继续经 Rust 私有 UDS 失败关闭，恢复只接受 Rust 返回的已验证 DTO。Go 保留紧邻硬件写入前的当前 IMEI 与 USB VID/PID 匹配，避免有效备份应用到另一台模块。

Rust 备份专项 **18项通过**，daemon 全量 **743通过、3忽略**；Go call-mode 定向 race、Go 主包全量 race 与 `go vet` 通过。迁移契约工具 **42项通过**，扫描保持 **264** 条（Rust **184** / Go **80**，含30个HEAD别名）、**210** 个审阅范围、**127** 个Swift调用点。Release `scripts/build.sh` 构建 2.86/build 286 arm64 `dist/Switch.app`，严格签名校验通过。没有操作读卡器或实体卡，没有生成DMG。

该备份服务校验边界计入P2A **+0.025计划人周**，至 **3.735/5.5（67.91%）**；整体为 **12.295/47.5≈25.9%（±3个百分点）**。Call-mode硬件状态机、其他Go业务、全Go-free构建/运行及设备级验收仍未完成。详情见[Iteration 121](../../iteration-121-progress.md)。


## Iteration 122：IMS/VoLTE 配置写事务迁入 Rust

`call_mode_voice` 现在由 Rust module-radio 在 CallMode lease 下执行：CLCC安全门禁、IMS/VoLTE读取、必要写入与重启生效前回读均为固定计划；终端拒绝停止，写入结果不确定时不重放。Go只调用typed action，不再写这两项QCFG；状态机、USB/ADB/helper及重启恢复仍留在Go。

同轮daemon **750通过、3忽略**；Go主包race与`go vet`、合同工具42项自测及普通契约扫描通过。此项计P2B **+0.025人周**，到3.850/8.0；总计12.320/47.5≈25.9%。详情见[Iteration 122](../../iteration-122-progress.md)。

## Iteration 123：IMS/VoLTE只读状态查询迁入 Rust

移除Go对IMS及VoLTE QCFG的直接读取/解析；Rust新增受reservation约束的只读typed action，调用者可复用CallMode/Call lease，否则申请并释放短CallMode lease。Rust完整daemon **752通过、3忽略**；Go主包race与`go vet`、Rustfmt、差异检查、迁移工具42项测试、264条契约扫描（Rust184/Go80，210审阅范围、127 Swift调用点）通过。Release `Switch.app` 2.86/build286 arm64 deep strict签名有效。产物仍含Go `switchd` 与Rust `switchd-rs`，全Rust目标仍未完成；无硬件测试、无DMG。

此项计P2B **+0.025人周**至3.875/8.0（48.44%）；整体为**12.345/47.5≈26.0%（±3个百分点）**。详见[Iteration 123](../../iteration-123-progress.md)。

## Iteration 124：call-mode USBCFG 查询迁入 Rust

Rust module-radio新增只读 `call_mode_usb_composition_read` 固定计划，在CallMode/Call lease内串行查询并校验唯一USBCFG响应、VID/PID和7个布尔位。Go通话模式状态、启用/还原预检、写后/重启后核验及接口就绪检查改用Rust typed action；Go原始USBCFG查询和响应解析器已移除，写入、ADB/helper和重启状态机仍在Go。

全量离线验证755通过/3忽略，Go race与vet、42项契约自测、普通契约扫描及Release应用构建/签名通过。P2B **+0.025人周**至3.900/8.0（48.75%）；总体**12.370/47.5≈26.0%（±3个百分点）**。无实机操作或DMG。详见[Iteration 124](../../iteration-124-progress.md)。


## Iteration 125：通话模式 USBCFG 写入迁入 Rust

通话准备与备份还原均改用 Rust compare-and-write action；Rust先检查通话状态及预期快照，写入一次后执行 typed readback，异常结果不会自动重放。Go保留目标生成、身份绑定、ADB/helper和重启状态机。

Rust workspace 759通过/3忽略，Go race与vet、42项契约自测、普通扫描和格式检查通过；264条契约归属未变（Rust184/Go80）。无实机写入或DMG。P2B **+0.025人周**至3.925/8.0（49.06%）；整体**12.395/47.5≈26.1%（±3个百分点）**。详见[Iteration 125](../../iteration-125-progress.md)。

## Iteration 134：模块语音运行时就绪缓存迁入 Rust

Go 的 `moduleVoicePrepared` 布尔缓存已移除。Rust 就绪状态绑定 producer、runtime generation 和模块 boot ID；daemon或模块重启、producer替换和旧代次请求均不会复用旧状态。部署成功需要 Rust 完成准备并提交 `prepared=true`，Go 客户端严格验证该字段。全量离线验证 Rust workspace 814通过/3忽略、Go race及vet通过、迁移契约42项自测和普通扫描通过。当前 census 为274项（Rust193/Go81，30个HEAD别名）、127个Swift调用点、221个审阅范围；Rust-only门禁仍因fallback及150条Go所有权/依赖路由失败。Release构建与限制见[Iteration 134](../../iteration-134-progress.md)。无实体设备操作或DMG。

P2B **+0.025人周**至**4.450/8.0（55.63%）**；整体为**12.920/47.5≈27.2%（±3个百分点）**。Go通话模式状态机和设备验收仍未完成。

## Iteration 135：通话模式只读状态分类迁入 Rust

Rust 新增私有认证接口，根据已有 USB/IMS/VoLTE/runtime 事实完成 readiness 分类和下一只读 ADB 探测选择；Go 保留状态缓存、观察、流程编排、设备动作与恢复。离线验证 Rust workspace 818通过/3忽略，Go race/vet、42项契约自测与清单扫描通过。最新清单275条（Rust194/Go81，30个HEAD别名）、127个Swift调用点、222个审阅范围。Release构建 `Switch.app` 2.87/build287 arm64 deep strict签名通过。Rust-only门禁仍未通过；没有实机操作或DMG。详见[Iteration 135](../../iteration-135-progress.md)。

P2B **+0.025人周**至**4.475/8.0（55.94%）**；整体为**12.945/47.5≈27.3%（±3个百分点）**。Go公开状态与更高层call-mode orchestration仍在迁移范围内。


## Iteration 136：模块语音运行时代次分配权迁入 Rust

Rust 私有认证接口现为当前 Go producer 分配运行时代次，Go 仅缓存 Rust 签发的令牌。离线全量测试819通过/3忽略；对应进度与构建见[Iteration 136](../../iteration-136-progress.md)。

## Iteration 137：IMS/VoLTE 就绪业务判定迁入 Rust

Rust 私有 `voice_facts` 投影现在拥有 IMS/VoLTE 就绪 predicate。它仅接受有效 voice tuple，拒绝 USB、运行时下载、探测等无关事实且不执行 I/O；Go 的来电准备、重启后确认和备份还原状态消费该 Rust 结果，投影不可用时失败关闭。全量离线验证 Rust 820通过/3忽略、Go race/vet通过、42项契约工具自测通过；清单276条（Rust195/Go81）、127个Swift调用点、224个审阅范围。Release `Switch.app` 2.89/build289 deep strict签名通过。Rust-only路由门禁仍报告Go fallback及150条Go所有权/依赖路由；未操作模块/读卡器/SIM，也未生成DMG。

P2B增加0.025计划人周等值至4.525/8.0（56.56%）；全项目为12.995/47.5≈27.4%（误差约±3个百分点）。见[Iteration 137](../../iteration-137-progress.md)。


## Iteration 138：模块通话接口就绪判断迁入 Rust

Rust 私有投影新增 `usb_interfaces` 与 `call_interfaces` 阶段，拥有 ADB/UAC 门禁及 ADB+UAC+IMS/VoLTE 的最终 readiness 合取判定。Go 保持 USB 先行、只在 ADB/UAC 就绪后读取语音配置的 I/O 顺序，并消费 Rust 返回的组合结果；既有 IMS 未就绪错误文案和失败关闭行为保留。离线验证 Rust workspace 822通过/3忽略、Go race/vet通过、42项契约自测通过；清单276条（Rust195/Go81）、127个Swift调用点、224个审阅范围。Release `Switch.app` 2.90/build290 deep strict签名通过。Go-only门禁仍报告fallback及150条Go owner/dependency路由；未操作模块/读卡器/SIM，也未生成DMG。

P2B增加0.025计划人周等值至4.550/8.0（56.88%）；全项目为13.020/47.5≈27.4%（误差约±3个百分点）。见[Iteration 138](../../iteration-138-progress.md)。


## Iteration 139：公开模块重启入口迁入 Rust

`POST /api/network/reboot-module` 现在由 Rust `module_radio` 直接处理，并在射频预约、飞行模式/业务状态门禁下执行。响应成功码和字段以及旧 coded-error JSON 保持兼容；Go mux与旧公开 handler移除。离线验证 Rust workspace 全量测试 **1,308通过、0失败、14忽略**（daemon 826通过、3忽略）、Go race/vet、42项迁移契约自测、普通census、Rustfmt及diff检查通过。当前276条契约（Rust196/Go80，含30个HEAD alias）、127个Swift调用点、224个审阅范围；Rust-only门禁仍报告默认Go fallback和149条Go所有权/依赖路由。Release `Switch.app` 2.91/build291 arm64 deep strict签名通过；没有实际模块重启、SIM操作或DMG。

P2B **+0.025人周**至**4.575/8.0（57.19%）**；总体 **13.045/47.5≈27.5%（±3个百分点）**。详情见[Iteration 139](../../iteration-139-progress.md)。

## Iteration 140：USBNet 模式接口迁入 Rust

`POST /api/network/usbnet` 现在由 Rust `module_radio` 执行，Go mux和handler移除。Rust只接受模式0/1，固定生成QCFG命令，在共享射频reservation/物理队列中以8秒命令预算执行，并确保成功、失败路径都尝试释放reservation；保留旧成功字段、400/502错误响应和1 MiB请求体上限。新增参数/动作/释放用例。

此轮还补齐旧 Go ServeMux 冻结方法样本，覆盖以前被遗漏的 Go 路由迁移项；Go生成的HTTP/HEAD响应与Rust对照 **350个wire case通过**、12个原样回落 case通过。完整迁移门禁通过：daemon 830通过/3忽略，Go全仓测试/vet与主包race通过，载波回放2722项以及USB状态、射频投影和PCMA/SDP/AMR对照通过；42项迁移脚本自测及census通过。276条接口为Rust197/Go79，127个Swift调用点，224个review范围。Release `Switch.app` **2.92/build292 arm64** deep strict签名通过；switchd-rs SHA-256 `db834cff70ad84ff22f2b344ebec887bb1983eabf284c8b9986d5f9c930951c5`。Rust-only门禁仍被默认Go fallback和148条Go所有权/依赖路由阻断。没有实际QCFG、模块、读卡器、SIM操作或DMG。

P2B **+0.025人周**至**4.600/8.0（57.50%）**；总体 **13.070/47.5≈27.5%（±3个百分点）**。详情见[Iteration 140](../../iteration-140-progress.md)。

## Iteration 141：公开网络诊断接口迁入 Rust

`GET /api/network` 的主机网络接口、默认路由、DJI USB 观察与五条固定只读 AT 查询现由 Rust 完成。AT 查询串行经过 `module_radio` actor，在一次 Diagnostic reservation 中执行并共享20秒期限；主机命令各自有输出上限和5秒期限。原Go mux注册、handler、PDP DTO/解析器已删除。Rust保留响应字段、原始命令结果和逐项错误，同时提供部分完成数、耗时与采集时间；安全事件仅记录阶段和粗粒度状态，不记录命令输出或主机细节。Go的其他可取消锁等待统一改用新 helper，Go race测试通过。

迁移门禁：Rust workspace **1,318通过、0失败、14忽略**；Go全仓测试与 `go vet` 通过；载波 **2,722** 项、USB状态、射频和codec parity通过；Go/Rust HTTP与HEAD逐字节对照 **359** 个原生案例、**12** 个回落案例通过；迁移工具自测 **42** 项、契约扫描通过。当前277条接口（Rust199/Go78，31个HEAD alias）、127个Swift调用点、225个审阅范围。Release `Switch.app` **2.93/build293 arm64** deep/strict签名通过；`switchd-rs` SHA-256 `6f3dd6128d1f595adc3bc3a74acad503ec004f734a9a8e04cabcea5b21330030`。没有实际AT命令、模块/读卡器/SIM操作或DMG。

P2B **+0.025人周**至**4.625/8.0（57.81%）**；总体 **13.095/47.5≈27.6%（±3个百分点）**。Rust-only门禁仍因默认Go fallback和147条Go所有权/依赖路由失败；离线测试及构建不代表实机网络诊断验收。详情见[Iteration 141](../../iteration-141-progress.md)。

## Iteration 142：网络服务与流量接口移除 Go health 依赖

`GET /api/network/services` 和 `GET /api/network/traffic` 现在从 Rust USB AT owner 读取活动设备描述；没有活动句柄时，使用本地有界 `ioreg` DJI 设备发现，不再请求 Go `/api/health`。保留网络服务 `module` 标记、无模块流量错误和设备字段兼容。此变更不打开额外 USB 句柄、不发送 AT、不改动模块或 SIM。

迁移门禁通过：`proletariusd` **836通过、3忽略**，Go全仓测试和 `go vet`、Rust workspace、网络路由/HTTP HEAD parity及codec/射频兼容检查通过；迁移工具自测 **42项**与契约清单检查通过。277条method/path（Rust199/Go78，31个HEAD alias）、127个Swift调用点、225个review scope。Rust-only门禁现仍被默认Go fallback和**143条**Go owner/dependency路由阻断。Release `Switch.app` **2.93/build293 arm64** deep/strict签名通过，`switchd-rs` SHA-256 `b7e6347054935fd2a814dd5b6509d74e0092f34ca88a49b3a4eb9f91aa86a4d5`。未做实体模块/读卡器/SIM验收，未生成DMG。

P2A **+0.025人周**至**3.760/5.5（68.36%）**；总量 **13.120/47.5≈27.6%（±3个百分点）**。Rust-only构建、Go退出和设备/长稳验收仍未完成。详情见[Iteration 142](../../iteration-142-progress.md)。

## Iteration 143：模块 Profile 备注编解码迁入 Rust

模块 Profile 备注的 CPBR 条目解析、URL-safe unpadded Base64 编解码、ICCID/重复记录处理、Unicode 长度与 255 字节记录上限现由私有 Rust codec 执行。Go 继续拥有公开备注 API、读卡器 AT 命令和硬件事务；Rust 不直接访问设备。错误信息、Go JSON 对无效 UTF-8 的替代行为和旧备注格式保持兼容。

Rust workspace **843通过、3忽略**；`scripts/test-migration-gates.sh`、Go 全仓测试/vet、42项迁移契约工具自测和 `--diff` 清单检查通过。当前279条method/path（Rust201/Go78，31个HEAD alias）、127个Swift调用点、226个审阅范围。Rust-only门禁仍因默认Go fallback及143条Go所有权/依赖路由失败。Release `Switch.app` **2.93/build293 arm64** deep/strict签名通过；内嵌 `switchd-rs` SHA-256 `8813532b694a3e4d46f559784566dfcecd4a6a5eac0c7e83f96a724f04b30841`。没有实体硬件操作或DMG。

P2B **+0.025计划人周等值**至**4.650/8.0（58.13%）**；总量 **13.145/47.5≈27.7%（±3个百分点）**。Rust-only构建、Go退出及设备/长稳验收仍未完成。详情见[Iteration 143](../../iteration-143-progress.md)。


## Iteration 145：模块 Profile 备注公开 API 与 ME 事务迁入 Rust

`GET/PUT /api/esim/module-notes` 现由共享 Rust radio actor 执行，旧 Go handlers/ME AT事务和中间codec RPC移除。读取按32槽批量进行；保存、更新、删除共享操作reservation并对未知写入结果禁止重放。完整 Rust/Go/HTTP parity门禁通过，契约更新为278条（Rust202/Go76，32条HEAD alias）、127个Swift调用点和226个review scope。Rust-only退出门禁仍因默认Go fallback及141条Go所有权/依赖route受阻；实机和最终Go-free验收未完成。详见[Iteration 145](../../iteration-145-progress.md)。

## Iteration 146：eSIM 电话簿探测迁入 Rust

`POST /api/esim/phonebook/probe` 由共享 Rust module-radio actor 接管；旧 Go handler/helper 删除。固定 AT 探测顺序、原 JSON/部分响应和 busy 语义保持不变，未确认的物理结果会停止后续命令。全迁移门禁通过：Rust workspace855通过/3忽略，Go全仓测试/vet及carrier、USB状态、radio policy、HTTP/HEAD和codec parity通过；42项工具自测与契约 `--diff` 通过。契约278条（Rust203/Go75、32个HEAD alias），127个Swift调用点、226个review scope。Rust-only门禁仍因默认Go fallback及140条Go所有权/依赖路由失败。来电按钮修正已通过Release构建与deep/strict签名验证；无硬件操作、未生成DMG。详情见[Iteration 146](../../iteration-146-progress.md)。

## Iteration 148：读卡器列表 Go 依赖审计补正

复核确认 Rust `GET /api/reader/list` 在 PC/SC 扫描后仍调用 Go 私有 `POST /api/internal/reader/reconcile` 获取 Go-owned 选卡与 BLE 状态；契约清单此前漏记该依赖。现已登记该依赖，HEAD alias继承它，并加入扫描器回归测试。43项契约工具自测、279条路由清单一致性检查通过；Rust-only退出门禁仍因默认 Go fallback 与141条 Go owner/dependency 路由阻断。本轮未改变生产 handler owner，不计迁移分，也未访问硬件。详情见[Iteration 148](../../iteration-148-progress.md)。

## Iteration 149：USB/PC/SC eSIM 健康读取迁入 Rust

选定 USB/PC/SC reader 的 `GET /api/reader/esim/health` 现由 Rust 读取 Profile 健康信息，并保留旧响应投影。HTTP 等待取消不会提前释放 PC/SC lease；worker 持有 lease，直到 APDU 事务结束且释放得到确认。reader manager generation lease/admission、完整 operation 投影、BLE/无 PC/SC及兼容回退仍依赖 Go。

仅已知 ISD-R AID 的 `6A82` 走既有分类 fallback；其他 APDU 错误失败关闭，`6982` 不重试、不绕过鉴权，9eSIM `6982` 仍排除。模块 Profile 备注的不确定写入也加了 fence，成功读取核对前拒绝再次写入，错误内容不回显 AT 记录。

Rust workspace 1,357通过/14忽略；Go全仓 `go test -timeout=30s ./...` 和 `go vet ./...` 通过；冻结 Go HTTP GET/HEAD parity、格式检查、契约 `--diff` 及43项工具自测通过。契约280条（Rust207/Go73，34个HEAD alias）、127个Swift调用点、237个review scope。Rust-only门禁仍因默认Go fallback与142条Go owner/dependency路由失败。

Release `Switch.app` 2.93/build293 arm64 构建及deep/strict签名通过；`switchd-rs` SHA-256 `0157ca248b6ff52c8b4b188c7198a85263f16238340fe7cf910ef6a23381cee6`。未操作实卡、未生成DMG。P2B +0.025计划人周至4.750/8.0（59.38%）；总体13.245/47.5≈27.9%。详情见[Iteration 149](../../iteration-149-progress.md)。

## Iteration 151：选定 USB/PCSC 读卡器 AKA 入口接入 Rust

选定且已就绪的 USB/PCSC reader 调用 `POST /api/reader/auth` 时，Rust 使用 generation-bound reader lease 和既有 PC/SC transaction 完成一次 AKA；RAND/AUTN/mode 校验顺序、成功字段与错误投影保持兼容，挑战材料清零。BLE、未选卡及提交前无法取得租约时仍调用 Go 兼容路径；APDU 提交后不回退、不重放，租约覆盖完整事务与确认释放。Go generation/session manager、BLE以及完整 VoWiFi 生命周期仍未迁移，因此不能称作端到端 Rust AKA 闭环。同一迭代还将IMS外发短信的MESSAGE构造、Call-ID/CSeq匹配、多段接受状态和超时unknown保护接入Rust；Go仍持有现有IMS TCP连接、SIP事件派发和会话生命周期，Rust私有UDS返回的只是待发送帧与状态，并不证明运营商已投递。

来电页呼入状态的接听/挂断按钮改为以通话卡片可用宽度为基准整组居中，固定按钮自然宽度，避免宽内容容器下偏左。Debug `Switch.app` 2.93/build293 编译通过，产物在 `../build/DerivedDataCallActionsFix/Build/Products/Debug/Switch.app`；未生成DMG。

`scripts/test-migration-gates.sh` 全量门禁通过：Rust workspace **890通过、3忽略**；Go全仓测试/vet、载波及USB/射频/codec parity、冻结HTTP/HEAD差分、43项契约工具自测及契约 `--diff` 通过。清单281条（Rust209/Go72，34个HEAD alias）、127个Swift调用点、248个审阅范围。Rust-only门禁仍因默认Go fallback和143条Go owner/dependency路由失败。最近一次App Debug构建早于IMS短信事务变更，本轮未重建App。未连接读卡器/实体SIM，未发送短信或访问运营商网络；9eSIM `6982` 未处理，未生成DMG。

本轮P2B与P3各计入 **+0.025计划人周**；总进度 **13.295/47.5 = 28.0%**（约±3个百分点），P2B **4.775/8.0 = 59.69%**，P3 **1.465/8.0 = 18.31%**。该切片仍保留Go租约与兼容回退；纯UI布局不计入迁移比例。

## Iteration 152：BLE 热插拔确认状态迁入 Rust

Rust 私有 reader-hotplug owner 承担 token/generation/observation-ID fencing 和连续 BLE 缺席确认；Go 仍提供 BLE inventory、在最终断开前复核 native lease 和 authMu、停止 VoWiFi 并清理 N 待代次。失败或不确定的 RPC 不会触发断开。该切片只迁移确认状态，不接管设备发现或 teardown。

全量 `scripts/test-migration-gates.sh` 通过；Rust workspace **1,389通过、6忽略**（proletariusd 896通过、3忽略），Go 全仓测试/vet、载波2,722项、USB/射频/codec与HTTP/HEAD parity通过；43项契约工具自测、契约 `--diff` 与 Rustfmt 通过。最新清单283条（Rust211/Go72，含34个HEAD alias）、127个Swift调用点、250个审阅范围。Rust-only门禁仍因默认Go fallback与**145条**Go owner/dependency路由失败。最近一次 `Switch.app` 2.93/build293 构建早于本轮后端变更，本轮未重建App。未连接实卡或操作读卡器，也未生成DMG。

P2B **+0.025计划人周**至**4.800/8.0（60.0%）**；总体 **13.320/47.5≈28.0%**（约±3个百分点）。UI修正不计入后端迁移进度。详情见[Iteration 152](../../iteration-152-progress.md)。

## Iteration 153：Rust 接管 N待入站事件归一化与 IMS 身份存储

Go N待 IMS回调现在只把收到的原始 RPDU/TPDU、解码结果、时间戳、状态报告和拼接元数据发送给私有 Rust store action。Rust按旧版 `multiID("mt", profile, RPDU)` 规则计算相同的 profile-scoped 去重ID，并完成耐久归档、multipart冲突隔离/安全回退和状态报告关联；回归覆盖网络重试、跨Profile隔离、拼接完成及冲突不误合并。Go IMS socket和事件确认生命周期仍未迁移。

Rust reader-device-identity owner同时负责兼容 `imei.json` 格式、原子私有写入及锁内首次随机生成。内部 resolver 在main dispatcher完成私有token校验，shutdown期间拒绝新建；Go VoWiFi仅在每次IMS session启动时请求当前身份，RPC失败返回空并停止继续注册。旧Go文件生成器仍保留在显式Go-only兼容路径。

全量 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,394通过、6忽略**（proletariusd 901通过、3忽略）；Go全仓vet/test、载波2,722项、USB状态/射频/codec/HTTP HEAD parity通过。契约 `--diff`、43项工具自测和Rustfmt通过。清单285条（Rust213/Go72，34条HEAD alias）、127个Swift调用点、251个审阅范围。Rust-only门禁仍因默认Go fallback与145条Go owner/dependency路由失败。未构建App、未生成DMG，未操作读卡器或SIM。

P2B **+0.025计划人周**至**4.825/8.0（60.31%）**，P4 **+0.025**至**0.475/4.5（10.56%）**；总体 **13.370/47.5≈28.1%**（约±3个百分点）。这不代表VoWiFi首次身份生成已实卡验收，也不代表N待网络持续恢复或多Profile仲裁已完成。详情见[Iteration 153](../../iteration-153-progress.md)。

## Iteration 154：入站短信 RP-ACK delivery-report 帧迁入 Rust

入站 SIP MESSAGE 仍由 Go 的已注册 IMS transport 处理：先立即返回 SIP 200，再解析 RP-MT-DATA 并调用当前耐久入库回调。回调成功后，Go 通过原认证 UDS transaction route 请求 Rust 构造单独的 delivery-report SIP MESSAGE。Rust生成 Call-ID、branch/tag、headers 与二进制 RP-ACK `[0x02, reference]` body，并在同一 IMS SMS session 下分配高于现有 floor 的 CSeq；Go把返回帧写入当前注册 TCP stream 一次，并在 Go event loop 中等待匹配的 SIP final response。旧路由、字段和 SIP/RP wire 语义保持不变，9eSIM `6982` 不在本轮处理范围。

这次迁移只把 delivery-report 帧与 CSeq 构造交给 Rust。入站 SIP 200、耐久回调/是否确认的决策、注册 socket/TCP stream、SIP 响应接收/匹配、Go session 生命周期、IMS 注册/VoWiFi/IKE、掉线恢复和真实运营商验收仍由 Go 或现有层负责；Rust actor不持有delivery-report的live response event。

`scripts/test-migration-gates.sh` 完整离线门禁通过：Rust workspace 1,398 passed、6 ignored（`switchd-rs` 903 passed、3 ignored）；Go全仓 vet/test通过；carrier replay 2,722 cases；USB status、radio、HTTP/HEAD、PCMA fixture、SDP与AMR parity通过。Rust migration contract `--diff`/一致性及43项工具自测通过，清单仍为285条（Rust213/Go72）、127个Swift调用点、251个审阅范围。Rust-only退出门禁仍被145条Go owner/dependency路径和默认fallback阻断。未构建App/DMG、未接读卡器/SIM或访问运营商网络。

P3按计划权重 +0.025人周至1.490/8.0（18.63%）；累计13.395/47.5人周（28.2%，估算误差约±3个百分点）。详情见[Iteration 154](../../iteration-154-progress.md)。

## Iteration 155：BLE eSIM 健康读取的 APDU 与 Profile 扫描迁入 Rust

选中的 BLE 读卡器现与 USB/PCSC 共用 Rust eSIM health/profile scanner。Rust 通过有界、鉴权的 Unix socket 调用现有 Swift CoreBluetooth bridge；每条 APDU 都携带预期 BLE reader identity，由 bridge 在调用 CoreBluetooth 前重新校验连接身份。Rust 一次只提交一个 APDU，不重试；APDU worker 持有既有 Go generation-bound reader-session lease 与 heartbeat，发现租约丢失后不再提交后续 APDU。桥接状态/配置未就绪且尚未发送 APDU时保留原 Go 兼容响应；已提交 APDU 的失败继续关闭处理。只将已知 ISD-R `6A82` 视为候选 AID 不存在；`6982` 不重试、不绕过鉴权，也不处理此前明确排除的 9eSIM 下载问题。

Rust workspace、Rustfmt、离线 fake-UDS/health-scan、契约 --diff 和工具自测证据见[Iteration 155](../../iteration-155-progress.md)。本轮新增2个reader BLE APDU私有action contract；当前清单287条（Rust216/Go71，含35个HEAD alias）、127个Swift HTTP调用点、254个审阅范围。当前迭代未改Go源码，未重跑Go全仓测试；USB-status/radio/HTTP方法差分fixture沿用其最近一次完整门禁记录。Go 仍负责 reader selection、generation/session admission/release、完整 BLE 生命周期和其他 BLE LPA 操作，所以这不是端到端 Go-free。未连接实卡或向硬件执行 APDU，也未生成DMG。

本轮按计划权重计 P2B **+0.025人周**至**4.850/8.0（60.63%）**；整体 **13.420/47.5≈28.3%**（约±3个百分点）。该权重只代表本切片的 Rust 语义迁移估算，并不代表设备验收。

## P2A：移除未引用的 Go 短信转发执行器

`backend/internal/smsforward` 无生产 Go 导入；Rust 已拥有转发状态、队列、provider 投递与 retry/unknown 策略，因此删除了重复 Go 执行器、控制器启动及五个兼容 fallback 注册。Rust route 和 Swift 调用保持不变，这项退役不计作生产职责迁移。保留历史 Go SWF1 fixture 的 Rust 兼容回归，以及仅验证共享 flock 的最小 Go interop peer。Rust 慢 provider 并发测试覆盖网络请求阻塞期间仍可查询/入队、在途任务不可取消、并发 flush 不重复投递。

P2A 独立分支的完整功能门禁、contract/HTTP 集成和 Swift suite 已通过；集成后清单刷新为 **280** 个 method/path contract（Rust **208** / Go **72**，含34个HEAD alias）、127个Swift HTTP调用点及237个审阅范围。该数值只反映路由扫描；reader/auth 与其他 Rust handler 仍有 Go compatibility/硬件依赖，Go-free 构建和实机验收尚未完成。本项不增加迁移完成率，也没有生成DMG或访问实体设备。

详情见[P2A执行器退役记录](p2a-forwarding-go-runtime-retirement.md)与[P2A旧路由切换说明](p2a-forwarding-legacy-go-cutover.md)。

## Iteration 156：IMS 入站短信解析与 BLE 操作入口迁入 Rust

Rust现解析IMS入站RPDU/TPDU并组装统一短信事件，覆盖普通短信、分段字段、状态报告和畸形/不支持内容的原始留存；Go侧解码器与回退路径移除。Go仍管理IMS SIP/socket/session事件顺序和持久化回调，并只在持久化成功后发送RP-ACK。BLE scan/connect/disconnect公开处理器迁到Rust，调用认证Go私有读卡器租约路由以共享硬件仲裁，再由Swift CoreBluetooth桥访问BLE设备，因此仍有明确Go依赖。

`scripts/test-migration-gates.sh`全量门禁通过：Rust workspace **1,428通过、6忽略**（`proletariusd` **929通过、3忽略**）；Go全仓测试/vet、载波2,722项回放、USB状态/射频/codec与HTTP/HEAD parity、43项契约工具自测和迁移契约`--diff`通过。定向Go race测试、Rustfmt和`git diff --check`也通过。最新清单288条（Rust219/Go69，含35个HEAD alias）、127个Swift调用点、257个审阅范围。`--require-rust-only`按预期失败：默认Go fallback仍在，145条路由仍含Go owner/dependency。无App重建、读卡器/SIM操作或DMG。

P2B **+0.025计划人周**至**4.875/8.0（60.94%）**；P3 **+0.025**至**1.515/8.0（18.94%）**；总体 **13.470/47.5≈28.4%**（约±3个百分点）。BLE接口仍通过Go租约，IMS耐久持久化与SIP生命周期仍由Go负责；没有把这些部分计算为已完成。详情见[Iteration 156](../../iteration-156-progress.md)。

## Iteration 157：IMS 入站短信解码与 N 待耐久入库合并到 Rust

新增已认证的内部 IMS transaction action：Rust解码入站RPDU/TPDU、组装短信事件并按profile写入N待耐久存储，在同一事务响应中返回`store_state`。Go的N待回调必须确认收到已持久化结果后才继续原有RP-ACK流程；普通读卡器IMS仍采用decode-only路径。Go不再序列化N待入站事件，也不再单独调用第二个存储RPC。SIP会话、注册流、事件顺序和RP-ACK传输仍由Go管理；未增加HTTP method/path。

Rust transaction测试覆盖首次入库、同profile重试去重、跨profile隔离和畸形RPDU不提交；Rust/Go进程离线集成15项检查通过。完整迁移门禁通过：Rust daemon 930通过/3忽略、Go全仓测试与vet、载波2,722项回放、USB/射频/codec/HTTP parity、迁移契约`--diff`及43项工具自测均通过。当前清单288条（Rust219/Go69，含35个HEAD alias）、127个Swift调用点、257个审阅范围。Rust-only门禁仍因默认Go fallback和145条Go owner/dependency路由而失败。未连接实体读卡器/SIM、未做在线IMS验收、未重建App或生成DMG。

P3 **+0.025计划人周**至**1.540/8.0（19.25%）**；累计**13.495/47.5≈28.4%**（约±3个百分点）。详见[Iteration 157](../../iteration-157-progress.md)。


## Iteration 158：BLE eSIM 操作、普通 IMS 短信归档与 N 待 Profile 投影

选中 BLE reader 的 Profile 改名和启用/切换 APDU 序列现由Rust执行，仍依赖Go generation-bound reader mutation lease/finalizer与Swift CoreBluetooth桥。`GET /api/multistandby/profiles` 的公开列表和offline/desired-state投影迁Rust，实时runtime snapshot仍经认证Go适配器，多Profile session/card arbitration仍由Go负责。普通reader/module IMS 短信现在由Rust完成解码和既有归档提交，Go只在Rust确认归档后继续RP-ACK；SIP会话/注册流仍由Go负责。

全量迁移门禁通过：Rust workspace（daemon 937通过/3忽略）、Go全仓test/vet、2,722项carrier replay、USB/radio/HTTP-HEAD parity、43项契约自测、迁移契约`--diff`及Rustfmt通过；真实Rust supervisor + Go子进程集成16项通过。清单290条（Rust221/Go69，36个HEAD alias）、127个Swift HTTP调用点、265个审阅范围。Rust-only退出检查仍被默认Go fallback和145条Go owner/dependency route阻断。未连接设备/运营商、未构建App、未生成DMG。

Iteration 158计入P2A和P2B各+0.025计划人周：P2A为3.785/5.5（68.82%），P2B为4.900/8.0（61.25%）；P3维持1.540/8.0（19.25%），P4维持0.475/4.5（10.56%）。总体13.545/47.5≈28.5%（估算误差约±3个百分点）。详见[Iteration 158](../../iteration-158-progress.md)。

## Iteration 159：BLE Profile 删除与 N 待卡片列表投影迁入 Rust

选中 BLE reader 的 Profile 删除现在与 USB/PCSC 共用同一 Rust LPA 删除、通知确认及 NVRAM 统计逻辑。Go 仍负责 generation-bound mutation lease、VoWiFi协调和最终释放；Swift CoreBluetooth仍是 APDU系统桥。租约授权后再次校验当前选中 reader，防止设备切换后误操作。未新增HTTP method/path。

`GET /api/multistandby/cards` 现在由 Rust分组投影；Go删除公开处理器，改为只提供认证的内部 runtime-snapshot，刷新物理清单并返回易变 reader/EID/可用性事实。`GET /api/multistandby/profiles`保持Rust catalog/desired-state投影。卡片和Profile实时会话、物理发现与多Profile仲裁仍由Go管理。

全量 `scripts/test-migration-gates.sh` 通过：Rust workspace中 `proletariusd` **937通过、3忽略**，Go全仓测试与`go vet`、2,722项carrier replay、USB/radio/HTTP-HEAD/codec parity均通过。新fake BLE删除测试驱动了完整删除APDU序列，并核对每条命令绑定选中reader及token；Rust/Go隔离进程集成 **17项**通过，包含卡片列表、Profile投影和普通IMS短信归档。迁移契约扫描与`--diff`通过；工具自测43项通过，清单291条（Rust223/Go68，37个HEAD alias）、127个Swift HTTP调用点、268个审阅范围。Rust-only门禁仍因默认Go fallback及147条Go owner/dependency route失败。另修复了卡片投影测试的显式类型推断构建错误。

保守计入P2B **+0.025计划人周**至**4.925/8.0（61.56%）**；总量从13.545增至**13.570/47.5≈28.6%**（估算误差约±3个百分点），待完成约71.4%。 P2A为3.785/5.5，P3为1.540/8.0，P4为0.475/4.5。未构建App或DMG，未连接实体读卡器/SIM、未开展真实IMS/运营商验证。详情见[Iteration 159](../../iteration-159-progress.md)。
## Iteration 160：BLE 通知读取与手动重试迁入 Rust

选中BLE reader的通知列表和手动重试现在使用Rust的共用LPA transport实现，并通过Swift桥将APDU绑定到选中reader UUID。Go仍保留generation-bound lease、heartbeat/finalizer、选择复核和缓存；无选中reader时保留旧Go兼容路径。

完整迁移门禁通过：Rust daemon 939通过/3忽略，Go全仓test/vet、2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity通过。Rust/Go隔离进程集成17项、迁移契约工具43项、`--diff`、Rustfmt与`git diff --check`通过。两个fake BLE测试分别验证空通知列表、缺失序号和APDU的UUID/token绑定；没有真实ES9+投递或实体设备验收。契约291条（Rust223/Go68，含37个HEAD alias）、127个Swift HTTP调用点、270个审阅范围。Rust-only门禁仍因默认Go fallback及147条Go owner/dependency route失败。未构建App或DMG。

保守计入eSIM/LPA **+0.025计划人周**，由1.100/5.5提升至**1.125/5.5（20.45%）**；总体由13.570增至**13.595/47.5≈28.6%**（误差约±3个百分点）。P2A 3.785/5.5、P2B 4.925/8.0、P3 1.540/8.0、P4 0.475/4.5维持不变。详见[Iteration 160](../../iteration-160-progress.md)。

## Iteration 161：VoWiFi 代理诊断路由迁入 Rust

`POST /api/reader/vowifi/proxy-check`现在由Rust处理，复用既有SOCKS5握手探测与SWU传输选择器；出口IP归属查询是4秒超时、64KiB响应上限的可选请求，失败不改变本地代理检测结论。保持旧响应字段，并明确不把本地握手/配置检查表述成ePDG UDP往返、IKE或IMS已就绪。Go公开handler及重复探测代码已删除；无新增HTTP契约。

全量迁移门禁通过：Rust daemon 946通过/3忽略，Go全仓测试/vet、2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity通过；Rust/Go进程集成18项通过（含旧空代理响应），定向Rust代理测试6项与并发ID唯一性测试通过、Go route/method parity、契约工具43项、`--diff`及Rustfmt通过。契约291条（Rust224/Go67，含37个HEAD alias）、127个Swift HTTP调用点、272个审阅范围。Rust-only门禁仍因默认Go fallback和147条Go owner/dependency route阻断。没有真实代理网络、ePDG、IMS或SIM测试，未制作App/DMG。

保守计入P2A **+0.025计划人周**，由3.785/5.5升至**3.810/5.5（69.27%）**；总量由13.595升至**13.620/47.5≈28.7%**（估算误差约±3个百分点）。P3维持1.540/8.0（19.25%）；其他阶段本轮不计分。详见[Iteration 161](../../iteration-161-progress.md)。

## Iteration 162：IMS OPTIONS事务校验与短信转发数据源解耦

Rust现在校验IMS OPTIONS最终响应与本次Call-ID、CSeq、方法是否一致；Go若没有Rust校验通道则失败关闭。心跳定时器、SIP socket和响应流仍由Go管理。短信转发producer直接合并读取Rust持久归档和USB预览，不再依赖Go子进程generation或重复volatile快照；部分IMS/模块短信仍从Go兼容会话进入持久归档，因此不代表实时IMS收件端已Rust化。

全量迁移门禁通过：Rust daemon 947通过/3忽略、Go全仓test/vet、2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity；18项跨进程集成、43项契约自测、`--diff`、Rustfmt与`git diff --check`通过。清单291条（Rust224/Go67，37个HEAD alias）、127个Swift HTTP调用点、272个审阅范围。Rust-only门禁仍因默认Go fallback与147条Go owner/dependency route受阻。未操作硬件/SIM或真实网络，未构建App/DMG。

P2A增至**3.835/5.5（69.73%）**，P3增至**1.565/8.0（19.56%）**；两项各增加0.025计划人周等值，总体**13.670/47.5≈28.8%**（估算误差约±3个百分点）。Go-free构建/运行、IMS session退出及实机验收仍未完成。详见[Iteration 162](../../iteration-162-progress.md)。

## Iteration 163：转发空账本恢复与跨进程未知结果防重放

首次启用短信转发时，无变化的空准入状态不再落盘为会被后续视为损坏的`[]`；既有合法空账本会安全归一化。新增loopback合成Bark进程测试：provider已收到请求、Rust尚未收到确认时强杀daemon，重启后任务进入`paused_result_unknown`，完整轮询周期不自动重放；显式重试才发出第二次请求并完成。非空账本和损坏账本仍按原规则保护。

本轮 `proletarius-store` 8项、Rust daemon 951项通过/3项忽略，19项Rust/Go隔离进程集成、契约`--diff`、Rustfmt和差异检查通过。Iteration162仍是最近一次完整迁移门禁；本轮没有重跑全套Go/carrier/parity。真实provider投递、系统PAC、设备及SIM验收尚未完成，也未生成DMG。

P2A增至**3.860/5.5（70.18%）**；总进度为**13.695/47.5≈28.8%**（估算误差约±3个百分点）。Go-free构建/运行和全部端到端验收仍未完成。详情见[Iteration 163](../../iteration-163-progress.md)。

## Iteration 164：BLE eSIM 概览读取与 PKI 资源归属迁入 Rust

选中 BLE reader 时，Rust 经现有 Swift CoreBluetooth/APDU bridge 读取 EID、eUICC信息/地址、Profile列表和可选eSTK产品字段；既有 JSON 投影与PKI issuer/accreditation识别保持兼容。PKI JSON 由Rust `proletarius-euicc` crate自有资源嵌入，与Go兼容副本校验一致。Go仍控制generation-bound reader lease/heartbeat/finalizer、操作快照和其他设备会话；未选中BLE或不适用传输仍保留兼容路径。

全量迁移门禁通过：Rust workspace daemon 951通过/3忽略，Go全仓test/vet、2,722项carrier replay、USB/radio/codec/HTTP-HEAD parity通过；Rust/Go隔离进程集成19项，契约工具43项和`--diff`通过。契约清单291条（Rust224/Go67，37个HEAD alias）、127个Swift调用点和272个review scopes。无实卡/APDU运营商验收，9eSIM `6982`仍排除，也未构建DMG。

P2B **+0.025计划人周**至**4.950/8.0（61.88%）**；累计**13.720/47.5≈28.9%**。详情见[Iteration 164](../../iteration-164-progress.md)。

## Iteration 165：短信转发生产者触发链路端到端覆盖

21项Rust/Go隔离进程集成中，新增两条合成事件流：Go兼容IMS coordinator写入N待耐久store后自动转发；Go生产`archiveSMSChecked`把设备短信写入Rust archive后自动转发。两类事件由Rust producer进入同一个loopback Bark端点，各接收一次，额外观察完整轮询周期无重复。fixture调用真实生产归档客户端和Rust认证UDS，不接触硬件，也不增加生产HTTP API。

Iteration164是最近一次完整migration-gates证据；本轮重建Rust daemon与Go fixture，21项跨进程检查和契约`--diff`通过，未重跑全套Go/carrier/parity。PAC、真实provider/设备与运营商验收仍未完成。

P2A **+0.025计划人周**至**3.885/5.5（70.64%）**；总进度 **13.745/47.5≈28.9%**（估算误差约±3个百分点）。详见[Iteration 165](../../iteration-165-progress.md)。

## Iteration 166：N待操作账本迁入 Rust

Rust持久管理N待start/stop/retry请求指纹、幂等键、操作状态及重启未知结果隔离；Go执行器开始设备/会话操作前必须取得Rust持久化单次claim，迟到/并发重复调用无法再次启动。执行结果未知不自动重放，Go仍负责真实Profile会话和物理卡生命周期。全量迁移门禁通过：Rust daemon 958通过/3忽略、Go全仓test/vet、2,722项carrier replay及USB/radio/codec/HTTP-HEAD parity；22项跨进程集成和8项定向操作账本测试通过。清单295条（Rust231/Go64，含38个HEAD alias）、127个Swift调用点、278个审阅范围。Rust-only仍有默认Go fallback及149条Go owner/dependency路由，未做实卡或DMG。

P4 **+0.025计划人周**至**0.500/4.5（11.11%）**；总进度**13.770/47.5≈29.0%**（估算误差约±3个百分点）。详情见[Iteration 166](../../iteration-166-progress.md)。

## Iteration 167：N待操作唯一执行领取迁入 Rust

Go不再保存进程内operation ID去重表；在Go校验当前卡片/Profile后，先向Rust耐久账本领取执行权。Rust原子地把pending转为running，并发或重复回调只有一个赢家；重启后的running转为unknown且不可再领取。停止期间claim被拒绝，终态transition仍可完成。Go继续执行设备/Profile会话，操作执行并未Go-free。

完整迁移门禁通过：Rust workspace 1,456通过/6忽略（daemon 959通过/3忽略），Go全仓test/vet、2,722项载波回放、USB/radio/codec/HTTP-HEAD parity通过；22项Rust/Go进程集成和8项定向Rust操作账本测试通过。契约295条（Rust231/Go64，38个HEAD alias）、127个Swift调用点、278个审阅范围。Rust-only门禁仍受默认Go fallback及149条Go owner/dependency路由阻断；未做实卡或DMG。

P4 **+0.025计划人周**至**0.525/4.5（11.67%）**；总进度**13.795/47.5≈29.0%**（误差约±3个百分点）。详情见[Iteration 167](../../iteration-167-progress.md)。


## Iteration 168：N待恢复退避策略决策迁入 Rust

Rust 私有 recovery-plan RPC 负责稳定在线窗口、连续故障计数、计划续期/重认证延迟、立即人工重试和意外故障退避。Go 只提交有界的计时事实，校验返回值后仍运行 timer 和 Profile 恢复；Rust IPC 失败时保留受限兼容回退。完整迁移门禁通过：Rust workspace 1,459通过/6忽略（daemon 962通过/3忽略），Go全仓test/vet、2,722项载波回放、USB/radio/codec/HTTP-HEAD parity以及23项跨进程集成通过。契约296条（Rust232/Go64，含38个HEAD alias），127个Swift调用点、278个审阅范围；Rust-only仍有默认Go fallback和150条Go owner/dependency路由。未操作实卡、未构建App或DMG。

P4 **+0.025计划人周**至**0.550/4.5（12.22%）**；总进度**13.820/47.5≈29.1%**。详情见[Iteration 168](../../iteration-168-progress.md)。


## Iteration 169：N待逐 Profile 恢复状态持久化到 Rust

Go不再向 recovery-plan RPC 发送本地失败计数或稳定在线影子值；请求改为Profile ID、原因码、观察到的在线时长和观测缺口。Rust按Profile在runtime journal中持久保存失败预算与稳定在线累计时长，并返回规范化原因码。旧journal的缺省字段保持可读，状态上限和持久化失败采取有界、fail-closed处理。Go仍执行计时器、Profile session、物理卡恢复，并保留IPC错误下的有界过渡回退。

全量迁移门禁、23项监督进程集成及契约`--diff`通过。P4增加0.025人周至0.575/4.5（12.78%）；总体增至13.845/47.5≈29.1%。该增量不包括N待session/broker、物理恢复和实机长稳验收。详情见[Iteration 169](../../iteration-169-progress.md)。

## Iteration 170：N待启动恢复轮询与代次去重归 Rust

Rust daemon 现在每 5 秒检查持久 `keep_online` 意图与 Go 后端就绪代次；只有存在意图时才调用现有认证 `runtime-snapshot` 适配器。读卡器不可用时重试，成功后每个 Go 代次只协调一次；Go 子进程或 Rust supervisor 重启会触发新一轮协调。Go启动ticker已删除，但Go仍拥有物理inventory/session启动、broker和逐Profile恢复timer。

Rust workspace 1,463通过/6忽略（daemon 966/3），Go全仓test/vet、26项监督进程集成、2,722项carrier replay及USB/radio/codec/HTTP-HEAD parity通过；契约清单296/127/277，`--diff`和43项工具自测通过。Rust-only出口及实机/长稳验收未完成。P4至0.600/4.5（13.33%），总进度13.870/47.5（29.2%）。详见[Iteration 170](../../iteration-170-progress.md)。

## Iteration 171：N待运行观察事实归一化迁入 Rust

Go只向私有 observation RPC 发送有界的时间、Profile ID、代次、原因码、`desired` 以及 tunnel、IMS、messaging、SIM短信路由就绪事实。Rust白名单归一化原因并推导规范状态与恢复标记，继续写入既有 Event journal 格式；公开报告及旧 journal 格式保持兼容。Go仍负责采集实体 IMS/隧道/SMS路由就绪事实与 Profile session。

Rust workspace 1,464通过/6忽略（daemon 967/3），Go全仓test/vet、26项监督进程集成、2,722项carrier replay及USB/radio/codec/HTTP-HEAD parity通过；契约清单296/127/277，`--diff`和43项工具自测通过。Rust-only出口及实机/长稳验收未完成。P4至0.625/4.5（13.89%），总进度13.895/47.5（29.3%）。详见[Iteration 171](../../iteration-171-progress.md)。


## Iteration 172：N待维护调度与短信入队迁入 Rust

Rust daemon持有N待每秒维护节拍，Go原先每Profile ticker已删除；认证Go adapter只向活动session worker发送可合并信号，Go继续执行flush、就绪采样和session/读卡器生命周期。公开 `POST /api/multistandby/messages/send` 的校验和耐久幂等入队由Rust负责；Go只提供当前reader generation/session/SMS通道的只读就绪事实。

完整迁移门禁通过：Rust workspace与format、Go全仓test/vet、2,722项carrier replay、USB/radio/HTTP method/HEAD、codec/SDP parity；Rust/Go隔离进程集成28项，迁移契约`--diff`及43项自测通过。清单298条（Rust233/Go65，38个HEAD别名）、127个Swift调用点、278个审阅范围。Rust-only退出、N待broker/session、多Profile仲裁、Go-free运行及设备长稳验收仍未完成。未操作实体读卡器/SIM或运营商网络，未构建App/DMG。

P4增加0.050计划人周等值至**0.675/4.5（15.0%）**；总体为**13.945/47.5≈29.4%**（估算误差约±3个百分点）。详情见[Iteration 172](../../iteration-172-progress.md)。

## Iteration 173：N 待逐 Profile 耐久重试时钟迁入 Rust

Rust runtime journal 持久化每个 Profile 的 reconnect backoff deadline、Go boot owner 与 attempt generation；重复请求保留既有deadline，陈旧代次拒绝，Rust dispatcher只在Go ready时投递到期事件。确定性0–999ms抖动分散同时故障的Profile。历史快照中曾由Go保留有界本地timer fallback；Iteration353已移除，当前失败调度会等待显式操作，Go仍负责重连worker与物理/session恢复。

完整门禁通过：Rust workspace/Go全仓test-vet、2,722项carrier replay、USB/radio/HTTP method/HEAD、codec/SDP parity；29项跨进程集成和迁移契约工具验证通过。该轮契约300条（Rust234/Go66，含38个HEAD alias）、127个Swift调用点和277个review scopes。

P4 +0.025计划人周等值至**0.700/4.5（15.6%）**；总进度**13.970/47.5≈29.4%**。详情见[Iteration 173](../../iteration-173-progress.md)。

## Iteration 174：IMS 注册续期 deadline 迁入 Rust

Rust journal将IMS REGISTER lease renewal deadline与N待重连backoff分开耐久保存；注册期限不加抖动以保留lease安全余量。认证到期callback绑定Go boot owner、Profile、desired/session状态与session generation，再经Go现有failure arbitration触发planned renewal。Go仍执行SIP/session与硬件恢复。历史fallback已在Iteration353退役；当前Rust scheduler失败时停止自动恢复，等待手动重试/停止。

完整门禁通过：Rust workspace 1,474通过/6忽略（daemon 975/3），Go全仓test/vet、2,722项carrier replay、USB/radio/HTTP method/HEAD、codec/SDP parity；30项跨进程集成，契约302条（Rust235/Go67，38个HEAD alias）、127个Swift调用点、278个review scopes，`--diff`及43项工具自测通过。Rust-only出口、N待broker/session、多Profile在线仲裁、Go-free运行及设备/长稳验收仍未完成。

P4 +0.025计划人周等值至**0.725/4.5（16.1%）**；总进度**13.995/47.5≈29.5%**。详情见[Iteration 174](../../iteration-174-progress.md)。

## Iteration 175：公开健康状态与 N 待重连代次迁入 Rust

Rust 网关现在直接生成 `/api/health` 的兼容响应，并以同一代次绑定的投影为 `device-snapshot` 提供健康字段。健康事实通过带内部认证的 Go observation adapter 获取；Rust 在调用前后验证 Go 子进程仍处于同一可用代次，并校验请求代次、响应 DTO 与必需字段。Go `GET /api/health` 在显式兼容启动模式中继续工作，因此本轮没有移除 Go 健康采集或 Go 运行依赖。

N待重连调度不再由Go分配generation。Go为每次尝试生成opaque token并提交有界delay，Rust负责单调generation分配以及schedule和counter原子持久化；恢复旧journal时从pending schedule播种counter。同token重复请求幂等、冲突复用拒绝。到期callback同时携带Rust generation与attempt token，Go只有当前desired runtime匹配时才唤醒worker。手动重试后迟到通知只会确认，不会缩短新一轮等待。Rust负责时钟、持久状态和回调栅栏；Go仍负责实际Profile/session恢复，Rust IPC故障时保留有界本地timer。

完整 `scripts/test-migration-gates.sh` 通过；daemon **982 passed/3 ignored**，Go 全仓 `vet`/测试、2,722 项 carrier replay、USB/radio/HTTP method+HEAD/codec+SDP parity 通过。Rust retry journal 21项定向测试和30项Rust/Go跨进程集成覆盖generation分配、旧journal恢复、重复/冲突请求与迟到callback；健康投影覆盖wire-shape、必需字段、代次不匹配和查询期间子进程重启。新增健康路由合同为 `HEAD /api/health` 与私有 `POST /api/internal/runtime/health-snapshot`。当前清单304条（Rust237/Go67，含39个HEAD别名）、127个Swift调用点、284个审阅范围，契约差异和43项检查器自测通过。`--require-rust-only` 仍被默认 Go fallback 和现有 Go owner/dependency 阻断；没有操作读卡器、号卡、短信或运营商网络，也未生成 App/DMG。

P2A健康投影计入 **+0.025 计划人周等值**，达到 **3.910/5.5（71.1%）**；P4重连代次/调度另计 **+0.025**，达到 **0.750/4.5（16.7%）**；总体为 **14.045/47.5≈29.6%**（估算误差约±3个百分点）。Go-free健康发现、设备观测、N待物理/session接管及最终构建验收仍未完成。

## Iteration 409–410 更新

Iteration 409 将生产 USSD 请求 wire 编码迁入 Rust；Iteration 410 将生产呼叫 INVITE/ACK/CANCEL/BYE/PRACK wire 编码迁入 Rust。Rust 负责有界字段校验、顺序序列化、Content-Length 和二进制 body；Go 仍负责请求事实、dialog/事务、SIP socket、响应、通话及 IMS 生命周期。Rust/Go parity、全量离线迁移门禁及365-scope契约 diff 通过；没有实机/运营商验收。3.08/build308 DMG 止于409。


## Iteration 413：生产拨号目标判断移除 Go fallback

现有认证 IMS codec operation 承担拨号目标校验、规范化、紧急号码禁拨和 USSD 短码拒绝；Go preflight 在状态/媒体变更前调用 Rust，并复用返回 URI 构造首个 INVITE。删除 Go 的重复生产规则与兼容 fallback，Rust 失效时失败关闭。Go 仍拥有 SIP/dialog/session、媒体和 socket 生命周期。全量迁移门禁通过：Rust workspace 1,421 passed / 3 ignored，Go 全包 test/vet、Swift BLE、2,722 carrier cases 及协议 parity 通过；契约 346 条（Rust286/Go60，49 HEAD aliases）、127 Swift HTTP calls、370 review scopes，44 项工具自测及 `--diff` 通过。Rust-only exit 仍有7类阻塞，无实机呼叫验收。P3 +0.025 至2.815/8.0，项目17.520/47.5=36.88%。最新 DMG 止于 Iteration412。详见[Iteration 413](../../iteration-413-progress.md)和[门禁日志](../../../../build/optimization/iteration-413-migration-gates.log)。


### Iteration 420：Rust 批量构造 DTMF RTP 数据报

Rust 收到一次按键事实及当前 RTP 状态后构造完整 RFC 4733 数据报批次，Go 校验后在现有 UDP socket 上按 20 ms 节奏发送；媒体热路径仍未逐帧调用 IPC。全量迁移门禁及契约 diff 通过；P5 为 0.275/8.0，全项目 17.695/47.5（37.25%）。Go 呼叫/媒体 owner 和7类全Rust退出依赖仍未消除。


Iteration 429: 普通 IMS 入站短信必须有 Rust store_state=archived 确认才发送 RP-ACK，N待 Profile 事件 observer 保留。OPTIONS 请求构造、响应观察和心跳状态决策由 Rust 接口提供，Go 保留 socket、计时器与生命周期执行。此次 Go 入站归档回退在正常生产路径已冗余，进度不计分；详见[Iteration 429](../../iteration-429-progress.md)。


## Iteration 434 审阅范围补充

`POST /api/call-mode/enable` 现由 Rust 接收并校验明确确认字段，限制请求体并通过固定的内部认证 Go 适配器进入既有流程。未确认时 Rust 在任何 Go RPC/模块操作前返回原有 400 错误；确认后 Go 仍复核通话状态、持有 call-mode/radio/USB 操作锁、备份配置、写入模块 IMS/VoLTE 与 USB 配置并负责复位、重连和恢复。Rust 合成 UDS 测试验证确认路径和拒绝路径，Go adapter 测试验证认证和严格 DTO。完整离线门禁通过；这只迁移公开准入，不代表通话模式硬件流程已 Rust-only。
