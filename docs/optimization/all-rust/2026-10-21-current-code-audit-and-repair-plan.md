# Switch 当前代码审查与全 Rust 后端修复方案

**Iteration 677 当前来源继承 3.48/build348：** 修复 i676 冷构建中并行 Rust 测试因进程级 umask 竞争导致的管理目录 0600 与 EACCES。i676 cold 失败且没有 App，原始证据保留；严格权限 5/5/1、正常权限 5/1 仅是定向 PASS。完整3925项来源域仅两处源码变更、530条选择性pin、677→676→675→672→671→670→669→668→666祖先链和26份历史文档严格核对。新同源i677 full cold、App/daemon生命周期与handoff、B01–B12 closure、离线重复构建、三轮审议与最终attestation仍待执行。49父项/115叶子/21场景类保持0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P0未闭合、P1–P4验收中、P5/P6开放，整体分母未冻结。运营商实网注册延后、六组用户排除项仍未验证。见[i677来源审阅](iteration-677-source-refresh-review.md)。

**Iteration 676 当前来源继承 3.48/build348：** 仅收录已审阅的socket_lifetime.rs 并发锁竞争修复、clean-generated.rb 及夹具的定点缓存白名单；i675 cold通过但daemon生命周期首轮失败，该历史失败未授予缺陷关闭。完整3922项来源域、530条选择性pin、676→675→672→671→670→669→668→666祖先链和23份历史文档严格核对。新同源i676 full cold、App/daemon生命周期与handoff、B01–B12 closure、离线重复构建、三轮审议与最终attestation仍待执行。49父项/115叶子/21场景类保持0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P0未闭合、P1–P4验收中、P5/P6开放，整体分母未冻结。运营商实网注册延后、六组用户排除项仍未验证。见[i676来源审阅](iteration-676-source-refresh-review.md)。

**Iteration 675 当前来源继承 3.48/build348：** 仅收录已审阅的startup_loopback.rs 冷门禁测试修复；i674 完整冷门禁在该组286通过/7失败后停止，没有构建正式App或授予缺陷关闭。完整3919项来源域、530条选择性pin、675→672→671→670→669→668→666祖先链和20份历史文档严格核对。新同源full cold、App/daemon生命周期与handoff、B01–B12 closure、离线重复构建、三轮审议与最终attestation仍待执行。49父项/115叶子/21场景类保持0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P0未闭合、P1–P4验收中、P5/P6开放，整体分母未冻结。运营商实网注册延后、六组用户排除项仍未验证。见[i675来源审阅](iteration-675-source-refresh-review.md)。

**Iteration 672 当前来源继承 3.48/build348：** 冷构建artifact-audit App inventory由明确capsule根计算`dist/Switch.app`，并以真实self-test绑定默认路径兼容、capsule路径和越界拒绝；接受方严格要求保持。完整3916项来源域仅该producer/self-test变化，530条选择性pin和672→671→670→669→668→666祖先链绑定，17份历史文档保持原字节。i670失败cold及i671独立synthetic receipts均留作历史，不重标本轮PASS。新同源full168/336、App/同daemon lifecycle/handoff、B01–B12 closure、offline repeat和最终review/attestation仍待完成。49父项/115叶子/21场景类保持0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P0未闭合、P1–P4验收中、P5/P6开放，整体分母未冻结。运营商注册暂缓和六组用户排除项不变。见[i672来源审阅](iteration-672-source-refresh-review.md)。

**Iteration 671 当前来源继承 3.48/build348：** 当前完整来源域逐项对比 immutable i670 capsule，3913项仅两份测试fixture变化；恢复/发布fixture移除额外7字节读取上限，原reboot断言增加已有状态诊断。530条选择性来源包含原528条及2条新pin，继承671→670→669→668→666，14份历史文档保持原字节。i670 cold publication四worker失败记录保留，当前只刷新来源元数据，不授予重跑PASS。新同源full cold 168场景/336子进程、App及同daemon lifecycle/handoff、正式B01–B12 closure、offline repeat和最终review/attestation仍待完成。49父项/115叶子/21场景类保持0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P0未闭合、P1–P4验收中、P5/P6开放，整体分母未冻结。运营商实网注册按用户要求暂缓，六组排除项仍UNVERIFIED_EXCLUDED_BY_USER。见[i671来源审阅](iteration-671-source-refresh-review.md)。

**Iteration 670 当前候选 3.48/build348：** 修复安全空旧锁对的metadata-only来源迁移、首次物理intent耐久撤销和exact publication child counter和Keychain读失败凭据保留；定向与独立审阅只覆盖绑定源码。i669原full cold因counter拒绝仍为失败历史，receipt replay不是重跑通过；3.47临时预览不抵最终验收。当前528来源行继承670→669→668→666，11份历史原字节保持。新同源cold全门/App、同daemon lifecycle/handoff、正式B01–B12 closure和三轮review/attestation仍待完成。正式49父项/115叶子/21场景类保持0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P5实际16叶子，P0未闭合、P1–P4验收中、P5/P6开放，整体分母未冻结。运营商实网注册按用户要求暂缓，六组排除项仍UNVERIFIED_EXCLUDED_BY_USER。见[i670来源审阅](iteration-670-source-refresh-review.md)。

**Iteration 669 当前软件验收推进：** i668冻结源码0074…的cold运行已失败/受控终止，不能发布或借用其候选。当前修复prepare超时必须join真实startup transport子任务的owner缺口，并把原ignored进程helper接入实际父进程矩阵门禁；定向证据仅属于所列源码。当前元数据继承668→666原字节，完整新cold全门禁/App、同daemon lifecycle/handoff、正式B01–B12 closure及三轮审议/attestation仍待完成。正式台账49父项/115叶子/21场景类，0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P5实际16叶子，P0未闭合、P1–P4验收中、P5/P6未完成，整体分母未冻结；不沿用83.3%或10/12。六组原软件排除项保持UNVERIFIED_EXCLUDED_BY_USER。见[i669来源审阅](iteration-669-source-refresh-review.md)。

**Iteration 668 当前权威口径纠正：** i667报告中的“整体83.3%、P5 10/12、P0–P4完成”没有冻结、逐项可审计依据，不可沿用。正式台账为49父项、115叶子、21场景类，0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P5实际16叶子（15 NOT_REVIEWED / 1 PARTIAL），整体分母未冻结。i667 DJI/CTExcel只验证指定调试daemon的IMS注册、约46秒观测及停止清理，不证明SMS、通话、音质、其他运营商或长稳。六类i666软件排除项继续为`UNVERIFIED_EXCLUDED_BY_USER`，不会整体转PASS。B01–B12已有专项支持，但P6软件harness的19项及guarded21项已通过，完整P6仍为false；当前同源全门、冷App、daemon生命周期、转发handoff、正式closure和三轮顺序审议/attestation尚待完成。P0未闭合、P1–P4验收中、P5语音/性能待验收、P6未通过；不报告项目百分比。本轮受控i668 successor保留i666原字节，见[i668来源审阅](iteration-668-source-refresh-review.md)。

日期：2026-10-21。唯一工作区：`/Users/StatTrak/Desktop/Switch_MAC/`；源码 `src/`，验证产物 `build/`，发行物 `dist/`。

本文件最初记录的是审查基线与修复方案；截至 2026-10-22 已实施多轮修复及软件复验。下方“初始基线”保留审查当时的数据，不代表当前源码状态。当前实施与验收状态见紧随本段的进度表及各 B 项更新。唯一工作区仍为 `/Users/StatTrak/Desktop/Switch_MAC/`；不操作原始项目副本。DMG 延后到全部代码与验收完成、连续三轮交叉审议及修正复测通过之后生成。

## 实施状态更新（2026-10-22，Iteration 652）

| 项目 | 当前状态 | 当前证据与未闭合项 |
| --- | --- | --- |
| B01 短信分片归档 | i649 同源码全门禁及冷构建通过；真实短信待验收 | i649 子进程写入 seq2 后直接 exit(86)，父进程验证 Drop 哨兵缺失、部分消息不转发，补片后唯一完整消息及重放字节幂等。定向 1/1、归档组 26/26，已纳入 i649 同源码全量软件门禁；不等于断电或真实网络投递。SIM World 入站短信和多段短信仍待验收。 |
| B02 eSIM 修改准入 | 软件准入、取消屏障及全量 Rust 门禁通过 | rename/delete 共用 retained Rust owner；busy、journal fault 时假 APDU 回调为 0，并验证操作期间 gate 持有到物理 owner 完成。定向 3/3 通过，日志见 [Iteration 612](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-612-profile-mutation-tests.log)。真实卡片非破坏性回归仍待做。 |
| B03 操作 owner 与取消 | 软件取消/退出回归通过 | 下载、切卡、rename/delete 由 retained Rust owner 持有准入、物理事务与未知结果收尾；新增“durable begin 前 waiter 取消”及“改名/删除 waiter 取消后 owner 继续”屏障测试。`reader_esim_download` 既有 58 项定向覆盖 begin 取消、owner 保留、shutdown drain、切卡结果与 lease 回收；Iteration 612 新增取消屏障及全量 Rust 门禁通过。真实卡片操作取消验收仍待做。 |
| B04 退出与模块 LPA | 软件 drain 顺序回归通过 | 退出先停止 eSIM admission、drain 已接受 operation，再请求 module final receipt。新增 shutdown coordinator 集成回归，证明 finalizer 不会在已接受 eSIM owner 退出前启动；Iteration 612 全量 Rust 门禁通过。真实读卡器暂停 LPA 时的进程退出验收仍待做。 |
| B05–B08 音频纯逻辑与代次 | Rust 处理与软件门禁通过，实机待验收 | `proletarius-audio-core` 覆盖饱和 PCM、跨块线性重采样、设备方向策略、恢复代次及读卡器麦克风高通/自适应噪声门/增益/限幅。`AudioRouteRecoveryTicketLedger` 现在使旧延迟重试在新恢复成功后失效；生产 ledger 的确定性回归通过。Iteration 616 的 Swift 黄金向量、audio-core、31 个通话状态模拟及 Release `.app` 通过。模块真机双向音频、频响/抗混叠、实时缓冲指标与 stop/route race 仍待验收。 |
| B09 转发代理错误 | 修复已实现，整 crate 回归通过 | provider 13/13、forwarding crate 61/61、队列锁进程测试 3/3 通过。新增 worker 测试证明 5 次 CONNECT 前失败后持久化为永久失败并停止。该测试发现并修复永久失败错误地要求 `retry_at` 的队列 bug。Hyper 丢弃 HTTPS CONNECT 状态码，无法可靠区分 407/403/502/503；统一做有限的连接重试，避免无限重试。已提交后的错误仍保持未知。 |
| B10 源码门禁/指纹 | 当前源码预检及发布身份回归通过；验收分母冻结未完成 | 当前预检为 294/294 Rust HTTP owner、0 Go owner、129 个 Swift HTTP 调用点、467 个审查指纹范围；`build.sh`、`release.sh` 与 CI 共用 source preflight。B12 回归现覆盖构建期间源码改变、App identity 不匹配、身份复核后源码改变三个窗口，全部拒绝发布；`test-release-flow.sh` 与 5 项 App identity 单测通过。115 个需求叶子、21 类场景的来源审议和 runtime map UNKNOWN/superseded 节点仍未收口，分母未冻结；接口覆盖不替代逐叶审议。 |
| B11 干净构建输入 | i650 冻结快照全门禁、冷构建及产物审计通过；i652 元数据纠正待同源码复验；外部 CI 未验证 | i650 digest `62f65cd6afb6880c831563aa1d268d992a7b0ac3aeaeb1ab60c190b8d59db54f` 的完整软件门禁 Rust workspace 3,974/0/9、Swift/BLE 合成回归与协议回放 297 次通过，隔离 Release 冷构建及 artifact audit 通过。见 [i650 软件边界](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-650-software-acceptance.md)。i652 只纠正后发用户范围与元数据，不能沿用历史 digest 作为当前构建身份。 |
| B12 构建身份 | i650 App/backend/source identity 通过；i652 当前摘要产物待核对 | i650 App 3.46/build 346、Rust 3.46.0、arm64，严格深度签名和嵌入源码摘要核验通过，无 Go 后端。候选绑定 `62f65cd6afb6…`；i652 元数据已改变整树摘要，后续构建须核对新身份。 |
| SIM World 实机 VoWiFi 探针 | 最新冷构建候选的连接、EID/Profile读取、IMS注册及清理通过；短信/通话未验收 | Iteration 626 使用冷构建 Rust daemon，验证 ESTKme-RED / SIM World（MCC/MNC 454/03）读取 1 个 EID 组和 10 个 Profile，隧道及 IMS 注册成功；停止 VoWiFi、断开读卡器、daemon 退出和隔离运行数据清理均通过。第一次探针因将运营商显示名误传为 MCC/MNC 而安全拒绝启动，使用 `SIM World` 重试通过。没有发送短信、拨号或切换 Profile。`sms_ready`/`call_ready` 是服务状态，不替代实际短信、通话、音质和长稳验收。报告见 [Iteration 626 当前源码只读探针](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-626-source-locked-final-simworld.json)。 |
| DJI 第一代 USB/QMI 模块 | i649 QMI 清理隔离已通过同源码全门禁；i650 状态投影已纳入全量软件门禁，实机未验收 | i650 活跃模块 VoWiFi 的直接 /api/status 快路径改用共享缓存投影，真实 HTTP 合成回归 1/1、投影单测 3/3 通过，确认 QMI enabled/未初始化字段与既有 ICCID、IMSI、CFUN 状态共存且不新增 AT 命令。见 [修复证据](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-650-dji-status-http-fix.md)。设备选择与 RNDIS AT 接口规则保持；本机未枚举 DJI 模块，CTL_SYNC、AT 响应、热插拔、VoWiFi/短信/通话仍待实体设备验证。 |
| BLE 协议纯逻辑 Rust 化 | 主协议算法与帧处理已迁入 Rust | Rust 覆盖 B1/NBLE、BeeSIM、CCID 和 legacy 的认证/状态、组帧、校验、续帧与分包重组；Iteration 628 全门禁中的 BLE 管理器 41 组虚拟时钟事件和 9eSIM NBLE/B1 认证向量通过。CoreBluetooth/Keychain/GATT 仍是平台适配；真实 B0/B1/ESTK 卡片验收未完成。 | 后发用户请求重开 B0/B1 密码、B0 BF71 与正常授权下载兼容性评估（含 6982 诊断）；旧排除已失效，受保护下载实际适用性未决。
| Iteration 623 Rust 编译告警审查 | 两项 dead-code 告警已按调用事实清理；未改变功能验收状态 | `start_native_tunnel` 仅供单元测试使用，已限定 `cfg(test)`；`into_sec_agree_continuation` 仓库内无调用点，已删除，SEC-AGREE 仍通过现有独立流程测试。IMS 122 项、VoWiFi 287 项及关联集成测试通过；两 crate Release 检查通过且无对应告警。全 workspace 其余 dead-code/unused 告警仍需逐项辨别用途，不能批量抑制。源码指纹及测试边界见 [Iteration 623 审查记录](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-623-rust-warning-review.json)。 |
| 总体验收 / 进度百分比 | 未冻结分母 | 不以接口数或已完成测试数推算整体百分比。P0 需求范围审议及冻结仍未闭合；P1–P6 的实体硬件、运营商、SMS、通话、长稳、外部 CI 与最终发行验收未全部通过。 |

### Iteration 634：冷构建测试路径修正与全门禁复核

在 `iteration-634-current-code-review` capsule 内首次执行完整迁移门禁时，前置 20 项 Go 退出测试、1 项冷构建审计自测及 52 项迁移契约测试通过；随后 P6 硬件边界的 5 项测试因 capsule 绝对路径过长，`AF_UNIX` socket 绑定触发 `OSError: AF_UNIX path too long`。这不是硬件策略测试失败，但会使深路径的隔离构建无法通过。

将 `scripts/tests/test_p6_data_roundtrip.py` 的合成 stage/socket fixture 放到 macOS 私有临时目录 `/private/tmp`，以随机私有目录承载 fixture；生产 sandbox 规则与断言未放宽，也未打开真实硬件或外网。修复后该测试 5/5 通过。当前源码上的 `scripts/test-migration-gates.sh` 随后通过：Rust daemon 3,061 passed / 0 failed / 8 ignored；原生 CLI 2/2、shipping process 5/5、恢复进程 3/3、supervisor 12/12，BLE 管理器 41 组虚拟事件、冻结协议 6,466 个直接案例/297 次回放均通过；Go 退出和 294/294 Rust 路由检查通过。首次失败及当前通过的完整日志分别保留在 capsule `migration-gates.log` 与 [`iteration-634-migration-gates.log`](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-634-migration-gates.log)。

第二次 capsule 的 Release 冷构建与 artifact audit 已通过，但在完整门禁阶段又发现隔离 PATH 未暴露已安装的 `cargo-fmt`/`rustfmt`；Rustup 工具身份已更新为同时注册 `cargo-fmt`、`rustfmt`，并将它们的解析路径纳入依赖指纹。最终同一源码快照的冷构建及完整门禁以 [`iteration-634-current-code-review-final2/artifact-audit.json`](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/iteration-634-current-code-review-final2/artifact-audit.json)、[`iteration-634-current-code-review-final2/execution.json`](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/iteration-634-current-code-review-final2/execution.json) 和 [`iteration-634-current-code-review-final2/migration-gates.log`](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/iteration-634-current-code-review-final2/migration-gates.log) 为准；这个自托管 Apple Silicon capsule 不是 GitHub-hosted CI 运行证明。USB 树仅枚举到 Apple host controller，未见 DJI/QMI 模块，因此本轮没有实机握手验收。未生成 DMG。

### Iteration 638：Rust test socket fixture 深路径修复

i638 使用 3,743 个源码文件、约 106 MB 导出源码和约 56 MB 校验锁定 Cargo 缓存完成离线 arm64 Release 冷构建；`artifact-audit.json` 通过，App/daemon/libusb 架构、ad-hoc 签名、无 Go 二进制和 source digest `79a6c0f8…` 均核对通过。该 digest 只对应 i638 导出快照。

i638 冷快照迁移门禁发现 11 项 `AF_UNIX` 路径超限；第一次测试只修正使用 `CARGO_MANIFEST_DIR` 的 fixture 根后，完整门禁仍有 8 项把 capsule 深路径直接写入 socket fixture。现已将四组剩余 fixtures 改用 workspace 临时目录，并在本机项目环境下对 managed-control 5 项、备份目录形态、原生模块监督、socket lifetime 做定向复测；串行命令通过。i638 的失败日志保留为 [`i638-full-gates/migration-gates.log`](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/i638-full-gates/migration-gates.log)，不把该次失败记作产品测试失败，也不记作全门禁通过。当时要求 i639 包含这些改动重新冷构建和全门禁；后续 i646 冻结快照的软件复验已通过，见下方证据边界。

同轮完成 B08 旧恢复 ticket 代次隔离及确定性 Swift harness；`test-call-audio-store.sh` 中 31 个异步通话场景、释放测试和 retry-fence 回归通过，audio-core 9/9 通过。B12 发布流程加入构建后源码和 App/backend identity 一致性核验与回归，5 项 identity 单测及 `test-release-flow.sh` 通过，预检仍为 294/294 Rust、0 Go。以上是软件证据；系统当前未枚举 DJI 第一代设备，USB/QMI 握手验收未通过也未失败，状态为未测。 |

### Iteration 639：supervisor socket fixtures 深路径修正

i639 的干净 Release 和 artifact audit 通过；完整迁移门禁运行到 `switch-voice-supervisor` crate 时，4 个测试因 capsule 路径过长触发有意 fail-closed 的 `supervisor_namespace_unconfirmed`。这不是关闭安全路径检查的理由。已把 unit 与 CLI 集成测试的私有运行目录迁至 `std::env::temp_dir()` 下短且唯一的 fixture；本地 crate 复测 12/12 unit、1/1 CLI 通过。i639 全门禁结果仍为失败，完整日志保留于 [`i639-final/migration-gates.log`](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/i639-final/migration-gates.log)；当时要求 i640 重新执行冷构建和完整门禁。后续 i646 冻结快照已通过，不能据此把原 i639 失败日志改记为通过。

### Iteration 646/648/649/650 冻结证据与 i652 当前范围边界

i646 冻结源码身份为 `f72dd7f865a6b98df0ad637342c80c65ad0c58b7a9006fa2e6904e840f8801e9`，其完整迁移门禁、隔离 Release 冷构建、产物审计及 3.46/build 346 身份核对均已通过。完整门禁证据见 [i646 migration gates](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-646-migration-gates.log)，冷构建见 [execution.json](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/iteration-646-current-code-final/execution.json) 和 [artifact-audit.json](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/iteration-646-current-code-final/artifact-audit.json)，汇总见 [i646 软件报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-646-current-software-evidence.md)。这更新了本文件早期“i639/i640 待复验”的当前状态，不改变历史失败记录。

i648 新增 B01 同进程磁盘归档恢复、PCM 纯逻辑 Rust 迁移与 QMI failed-open 清理修复后，已冻结 digest `762ec0e4a77dfb765bd8579063403d3ea27756f46512970ec66b2facf95a885a` 并完成同源码完整迁移门禁、隔离 Release 冷构建、artifact audit 和身份复核。Rust workspace 3,970 通过/0 失败/9 忽略，BLE 42 组、Swift 通话模拟 31 场景、协议回放 297 次通过。见 [i648 软件报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-648-current-software-evidence.md) 与 [artifact audit](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/iteration-648-current-code-final/artifact-audit.json)。i649 B01 独立进程退出回归定向 1/1、归档组 26/26 通过；QMI reopen 的 abort/USB close 失败收尾已修复，包含活跃 retry/status 的定向组 42/42 通过，i649 同源码全门禁、冷构建与身份复核已经通过，冻结 digest 为 `712344ea8217…`，见 [i649 软件边界](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-649-software-acceptance.md)。后续 i650 状态投影修复已通过同源码完整门禁、冷构建及身份核验，冻结 digest 为 `62f65cd6afb6…`；i652 当前只纠正后发用户范围及元数据，仍须同摘要复验。外部 GitHub-hosted CI、DJI 实机 QMI、运营商业务、P0–P6 完整验收和最终三轮交叉审议仍未闭合，未生成 DMG。

Iteration 609 的 `dist/Switch.app` 曾完成 Release 构建和 ad-hoc 深度签名检查；它早于本轮构建门修复，不作为当前源码的最终发行物。核验记录见 [Iteration 609 验收报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-609-code-repair-verification.json)。

Iteration 610 对干净构建路径做了隔离舱重建并关闭 B11 软件构建缺口；完整测试原始日志见 [migration gates](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-610-migration-gates.log)，冷构建审计见 [cold build artifact audit](/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/iteration-610-b11-cold-build-r3/artifact-audit.json)。冷构建身份为 3.46/346、`com.proletarius.switch`，arm64；后台摘要 `3aa26e212d3343b41ed5e9992a8ea635071a23bf86819c79c9da67ecdf14be46` 与干净导出和当时主源码树一致。此项仅证明软件可冷构建和来源可追溯。

Iteration 611 的新增探针测试随完整迁移门禁通过：Python 99/99、Rust workspace 3,925 passed / 0 failed / 9 ignored、冻结协议回放 297 次，日志见 [Iteration 611 migration gates](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-611-migration-gates.log)。随后对 archive I/O、短信归档及 IMS 呼叫共 220 个定向测试复跑通过；为消除其中三个无用局部变量/捕获告警，代码做了不改变行为的整理，定向结果见 [archive I/O](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-611-archive-io-targeted.log)、[短信归档](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-611-message-archive-targeted.log)、[IMS 呼叫](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-611-native-ims-targeted.log)。

Iteration 612 的 post-probe 全量迁移门禁通过：Python 100/100、Rust workspace 3,927 passed / 0 failed / 9 ignored、冻结协议回放 297 次、oracle 完整性 6,466 组，详见 [post-probe migration gates](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-612-post-probe-migration-gates.log)。最新 eSIM mutation 屏障定向测试 3/3 通过。Release `.app` 当前源码摘要构建和产物核验通过，记录见 [build log](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-612-release-app-build.log) 与 [artifact verification](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-612-release-app-verification.json)；本轮没有生成 DMG。SIM World 实机复探使用同摘要候选注册成功，报告见 [SIM World probe](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-612-simworld-reader-vowifi.json)。此构建不是交付 DMG 的最终版本；三轮审议若修改代码，必须重新全量测试、构建和验证身份。Xcode 27 报告了 Swift API 弃用提示；Rust daemon 仍报告 149 条生产编译 warning，包含 dead-code/unused 项。N待 retry worker 已确认从 Rust daemon 启动并直接向 native service 派发；未调用的旧内部 HTTP 调度 handler 不代表实时重试链断开。剩余 warning 将逐项判断真实调用链和迁移边界，禁止批量 suppress 或仅按 lint 名称删除。
Iteration 626 的全量迁移门禁通过：Rust workspace 3,057 passed / 0 failed / 8 ignored，原生进程测试 5 项、恢复进程测试 3 项、BLE 事件组 41 项、协议夹具 6,466 组/297 次回放；Go backend exit 与 294/294 Rust 路由检查通过。台账结构检查通过（115 个需求叶子、21 类场景，分母未冻结）。当前冷构建摘要与 SIM World 探针详见候选验证报告；ESTKme-RED/SIM World 454/03 读取 1 个 EID 组、10 个 Profile 并注册 VoWiFi 成功，已正常停止和清理。未测试短信、通话或切换 Profile；未生成 DMG。详细证据见 `/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-626-retired-runtime-http-gates.log`、`/Users/StatTrak/Desktop/Switch_MAC/build/clean-build/iteration-626-source-locked-final/artifact-audit.json`、`/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-626-source-locked-simworld.json`、`/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-626-current-candidate-verification.json`。

Iteration 626 收口后完成当前冷构建与 SIM World 只读探针；Rust workspace 3,057 passed / 0 failed / 8 ignored，原生进程测试 5/5，恢复进程测试 3/3，BLE 事件组 41/41，冻结协议夹具 6,466 组/297 次回放；Go backend exit 与 294/294 Rust 路由检查通过。当前候选读出 1 个 EID 组和 10 个 Profile，成功注册并清理；没有测试短信、通话或切换 Profile。源码快照及历史锚点已更新，台账仍为 115 个需求叶子、21 类场景，分母未冻结。逐项 P0–P6 验收与三轮交叉审议尚未完成，因此不生成 DMG。证据见 [Iteration 626 源码清理记录](iteration-626-retired-runtime-http-cleanup.md)、[/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-626-retired-runtime-http-gates.log](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-626-retired-runtime-http-gates.log)。

结论：当前已达到“HTTP 后端接口全部归 Rust、无 Go 构建和运行依赖”的结构性状态；尚未达到“所有可移植后端逻辑都由 Rust 承担且全功能验收完成”。本轮整理出 **9 项功能或可靠性问题、3 项构建及诊断问题**，以及蓝牙协议、音频纯逻辑两类 Rust 边界收口工作。未发现的缺陷不能因此视为不存在。

## 1. 当前基线与证据边界

| 项目 | 审查起始基线（历史） | 能说明什么 |
| --- | --- | --- |
| Go 退出检查 | `check-rust-backend-exit.py --require-go-free`，exit 0 | 当前扫描范围内没有 Go 构建、打包、启动、派发或源码导出依赖 |
| 接口契约检查 | `check-rust-migration-contracts.py --require-rust-only --diff`，**exit 1** | 仍统计到 294/294 Rust owner、0 Go owner；但审阅指纹和目录未同步，不是有效的全绿发行基线 |
| 接口计数 | 294 条 method/path，含 54 条派生 HEAD；129 个 Swift HTTP 调用点；470 个指纹审阅范围 | 只说明接口归属；HEAD 别名不额外增加迁移成果 |
| 验收台账检查 | exit 0，`metadata_consistent=true` | 仅文档元数据一致；分母未冻结、产品未完成、未授予功能验收通过 |
| 台账口径差异 | 台账 Swift 条目 131，当前扫描调用点 129 | 必须核对条目语义、是否历史实例及适用范围；不能直接删掉两项让计数一致 |
| 版本 | App `VERSION=3.46`、`BUILD_NUMBER=346`；Rust workspace crate `3.45.0` | 发行身份尚未统一，详见 B12 |
| 离线音频探针 | 证实 Float32→Int32 满幅越界、重采样输出依赖分块 | 是数值实证，不是实机通话质量测量 |
| 离线转发探针 | 本机关闭端口作为显式代理，实际 TelegramSender 返回 `Unknown` | 没有建立代理连接也被停止自动重试，详见 B09；没有向外部 provider 投递 |
| 干净构建前置探针 | 空 ARM sysroot 被准备脚本拒绝；CI 未准备该组件 | 与 build.rs 必需输入调用链共同说明 B11；本轮未运行 GitHub CI |

本轮命令、exit code、22 个相关源文件的 SHA-256 见 [审查证据清单](/Users/StatTrak/Desktop/Switch_MAC/build/reports/code-audit-2026-10-21/summary.json)。原始输出：[契约检查](/Users/StatTrak/Desktop/Switch_MAC/build/reports/code-audit-2026-10-21/migration-contracts.log)、[Go 退出检查](/Users/StatTrak/Desktop/Switch_MAC/build/reports/code-audit-2026-10-21/go-backend-exit.log)、[验收台账检查](/Users/StatTrak/Desktop/Switch_MAC/build/reports/code-audit-2026-10-21/acceptance-ledger.log)。

历史证据单列：Iteration 603 曾有 daemon 3036 项及 workspace 其余 849 项通过；605 的 eSIM 定向测试 51 项通过。Iteration 604 曾对 ESTKme-RED/SIM World（454/03）完成一次读卡、22 秒内 IMS 注册、正常停止退出。本轮没有重跑上述全量测试或实机，也不能从注册成功推导收短信、通话、其他运营商或长稳均成功。

## 2. 缺陷清单与修复验收

“高/中”表示本方案的修复顺序，与项目 P0–P6 阶段无关。静态确认表示当前代码存在明确触发路径；仍应先补确定性复现，再实施修复。

### B01｜高：普通读卡器/模块的 IMS 长短信可能缺片

普通读卡器和模块分支将已解析的短信归档时，丢弃拼接引用、总段数、分片序号；N待分支保留这些字段。归档去重键使用设备、订阅、发送者、正文和时间，因此同一长短信中两个正文相同、SCTS 相同的不同分片可能被当成重复消息丢弃。归档仍返回成功，随后发送 SIP 200/RP-ACK，网络可认为分片已接收。

依据：[归档入口](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/ims_sms_transactions.rs:905)、[普通归档字段](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/ims_sms_transactions.rs:1006)、[N待保留 concat](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/ims_sms_transactions.rs:1071)、[去重键](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/message_archive.rs:753)、[入库后确认](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/native_sms_consumer.rs:400)。本项为静态调用链确认；不代表已复现用户当前验证码丢失的原因。

修复：统一 Rust 入站分片存储和组装；以订阅、发送者、拼接引用及位宽、总段数和有效时间窗口隔离消息，以 sequence 区分分片；分片耐久化后才确认，完整消息进入通知和转发。持久化失败不做成功 ACK；不完整消息明确保留状态，避免静默消失。普通短信和旧归档的读取保持兼容。

验收：乱序、重复、相同正文不同序号、跨订阅同引用、引用复用、重启恢复、磁盘写失败、缺片超时。必须证明相同正文的两个合法分片都保留、重复分片不多发通知、完整消息只进入一次转发准入。

### B02｜高：改名/删除没有使用统一 eSIM 安全准入

读卡器改名、删除只检查 `active()`。该函数不检查 journal 落盘故障，且重启后“上次操作中断、结果未核对”的状态为 failed，不属于 active。完整 `acquire_operation_gate()` 才拒绝这两类情况。

依据：[active 与完整 gate](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/reader_esim_download.rs:367)、[改名](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/reader_esim_download.rs:2442)、[删除](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/reader_esim_download.rs:2977)。底层 reader lease 仍提供物理串行保护，不能据此声称已证明 APDU 并发或卡片损坏。

修复：所有 eSIM 修改都先获得统一 Rust permit，再获得设备 lease；改名/删除把整段操作交给 retained Rust owner，HTTP 等待者取消后 owner 仍持有 permit 到物理释放、最终刷新和状态核对结束。未知结果未核对时拒绝后续写卡，避免各接口自行拼装不同准入条件。

验收：恢复 interrupted 日志、注入日志写失败后，rename/delete 均被拒绝且假 APDU 回调数为 0；并发提交 reader/module mutation 由同一准入机制裁决。Iteration 612 fake 回归进一步验证 waiter 取消后 owner 完成一次操作、持锁直到结束；这不是实体卡 APDU 证据。

### B03｜高：切卡及下载预处理取消后可能留下无执行者的 running

切卡 durable begin、物理事务、会话恢复及终态写入依附 HTTP handler future；当前退出会 abort HTTP tasks，begin 后取消可能跳过 finalize/finish。下载虽已有独立 worker，但在 blocking journal begin 已开始、HTTP 尚未取得返回值并建立 worker 时仍有较窄窗口。可能留下没有执行者负责收尾的 running，造成持续 Busy，或物理操作结果与 UI 身份没有完成核对。

依据：[下载 begin](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/reader_esim_download.rs:1927)、[独立 worker 建立](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/reader_esim_download.rs:1975)、[切卡 begin](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/reader_esim_download.rs:3734)、[切卡完成](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/reader_esim_download.rs:3984)、[取消 HTTP tasks](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/main.rs:1134)。不假定每次客户端断开都会取消 handler；已确定的触发是任务被取消。底层 blocking 事务仍持物理 guard，不是“取消立即打断 APDU”。

修复：由受管理的 Rust operation owner 覆盖准入→日志 begin→物理事务→核对→release→终态；HTTP 只等待结果。rename/delete 现在使用同一 retained owner，确保已接纳的物理操作不随 HTTP waiter 取消而释放准入。分别处理未提交、已提交但结果未知、完成三种状态。605 已修好的下载 worker gate 生命周期保留，不重复改回。

验收：下载分别覆盖“blocking begin 尚未派发”和“begin 已执行、durable running 已落盘但独立 owner 尚未接管”两个窗口；前者不得提交 APDU 或创建活动操作，后者必须有 retained owner 收尾。再在切卡物理事务中、最终刷新前设置屏障并取消等待者；确认不存在无人收尾的 running，物理 release 与终态各执行一次，未知结果不会自动重新写卡。

### B04｜高：模块下载与退出并发时可能永久等待

下载独立任务没有被统一登记并 join。shutdown 在 shared 清理后并行关闭 module/reader/QMI；module radio 的最终回执可能看到 LPA reservation 仍存在，将暂时未完成永久缓存为 Err。即使下载随后释放 reservation，外层每秒确认仍只读到同一旧 Err。

依据：[独立 module download](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/reader_esim_download.rs:2210)、[关停顺序](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/shutdown.rs:125)、[最终回执缓存](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/module_radio.rs:1568)、[reservation 判断](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/module_radio.rs:1594)、[重复等待](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/main.rs:1217)。这是静态可达路径，未进行实体模块复现。

修复：停止新 eSIM 准入；对已接受 operation 进行可追踪 drain/join；完成后才计算最终物理释放回执。将“仍在执行”与“真正未知物理结果”分开，保留后者的保护，不能通过超时强杀、返回假成功或重复恢复写入消除等待。

验收：fake module reservation + 暂停的 LPA task，启动真实 shutdown coordinator；任务未释放不得宣称退出完成，随后释放能够完成退出，日志终态已落盘。另验证真正未知写入仍明确保持未确认。

### B05｜中：32 位 PCM 正满幅转换可触发运行时 trap

[AudioBridge.swift:485](/Users/StatTrak/Desktop/Switch_MAC/src/app/Sources/AudioBridge.swift:485) 使用 `Int32(clamped * 2147483647.0)`；Float32 将该常量舍入到 2147483648，`clamped=1` 时超出 Int32 范围。触发条件是使用 32 位整数 PCM 输出且出现正满幅样本；16 位路径不触发。

修复：将样本转换作为 Rust 音频纯函数，采用足够精度的中间运算和饱和转换，定义 NaN/Infinity 处理。验收 ±1、边界邻值、溢幅及支持的 PCM 位宽；不需要真实拨号即可覆盖崩溃条件。

### B06｜中：流式降采样跨块相位丢失

[AudioBridge.swift:524](/Users/StatTrak/Desktop/Switch_MAC/src/app/Sources/AudioBridge.swift:524) 实际移除 `min(consumed, buffer.count)`，却减去全部 consumed。48k→8k、同一 1024 样本一次处理得到 171 个输出，拆为 512+512 得到 172 个；第 86 项分别为 516 和 512。输出依赖输入分块，存在时序漂移和边界不连续。

修复：首先保持正确的剩余相位；迁入 Rust 后以流式重采样状态机管理分块。抗混叠滤波作为独立质量项测量，不能只改样本数便声称音质全面修复。验收随机分块与整块输出一致、长期样本数、频率响应和有界延迟。

B05/B06 的[数值探针](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-606-audio-numeric-probe.swift)与[结果](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-606-audio-numeric-probe.log)已保存；探针只进行数值运算。它们属于 USB 模块音频桥，不能直接解释 SIM World 读卡器路径的历史杂音。

### B07｜中：模块音频防回环检查方向写反

[AudioBridge.swift:226](/Users/StatTrak/Desktop/Switch_MAC/src/app/Sources/AudioBridge.swift:226) 比较 `speaker != moduleIn`、`mic != moduleOut`；实际 moduleIn 是输入，moduleOut 是输出。系统默认输出选择模块 AS/moduleOut，或默认输入选择模块 AC/moduleIn 时，保护可能放行，导致设备角色复用、回送或本机无声。

修复：按输入/输出方向排除模块端点，并按物理设备身份配对；设备选择规则归 Rust，Swift 只枚举系统端点。验收正常四端点、默认误选模块、输入输出共用设备 ID、多模块组合，避免仅交换两个比较后遗漏同物理设备的其他端点。

### B08｜中：旧通话延迟恢复可能干扰新通话

[ReaderVoWiFiAudioBridge.swift:621](/Users/StatTrak/Desktop/Switch_MAC/src/app/Sources/ReaderVoWiFiAudioBridge.swift:621) 的 400ms 延迟任务未绑定 sessionGeneration。旧会话失败后安排重试，停止并快速建立新会话，旧任务看到 `running=true` 就可重启新会话 engine，清空播放队列和 microphone tap。

修复：恢复请求带 session generation，执行前再次核验；stop 撤销待执行恢复。跨语言设备调用也必须校验代次，避免 Rust 决策正确但 Swift 回调作用到新设备。验收受控时钟中的“旧重试→stop→新会话→旧定时器触发”，新会话引擎、队列及播放代次均不得改变。

### B09｜中：代理未建立连接也会把转发锁为结果未知

Telegram/Bark 的 `.send()` 错误一律归 Unknown，worker 保留 result_unknown 并停止自动重试。本机关闭端口作为显式代理时，真实 TelegramSender 同样返回 Unknown，尽管代理连接都未建立、请求不可能到达 provider。网络恢复后消息不能自动补送。

依据：[Telegram 错误分类](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletarius-forwarding/src/provider.rs:214)、[Bark 对应分类](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletarius-forwarding/src/provider.rs:378)、[Unknown 保留](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletarius-forwarding/src/worker.rs:412)。这是防重复投递策略过度保守的可靠性缺口；提交后丢响应仍应保持 Unknown。

修复：当前只对 reqwest 明确归类为连接建立失败的请求做有限退避；提交后的 timeout/reset 继续标为未知并要求显式核对。实测 Hyper/reqwest 会把 HTTPS CONNECT 407、403、502、503 都折叠成 `is_connect=true`、`status=None`，因此无法安全给它们不同的配置提示；这些情况统一限 5 次后暂停，提示 `delivery_connect_retry_limit_reached`。保留系统/自定义代理，不加直连兜底或绕过代理自行解析 DNS。

验收：provider 与 worker 测试已覆盖 CONNECT 前失败（含合成 502/503/407/403）、拒绝连接、已提交 POST 后 5xx、Telegram/Bark 确认、Bark 加密及代理路由；forwarding crate 61/61 与队列锁进程测试 3/3 通过。五次连接错误后永久暂停的持久化集成测试通过。代理 CONNECT 状态码折叠是已记录限制；provider 已收到 POST 后 5xx/丢响应仍保留未知状态，避免重发。

### B10｜高：当前源代码审阅门禁失败，本地发行入口未强制执行

本轮契约检查明确失败：`reader_esim_download.rs:start` 和该生产文件整体指纹变化，契约目录行号也有漂移。主要关联上一轮 605 修改未同步的审阅记录。独立 Go 退出检查通过，不能抵消此失败。

[release.sh:57](/Users/StatTrak/Desktop/Switch_MAC/src/scripts/release.sh:57) 直接构建、签名、打包；其构建脚本没有强制执行迁移契约/Go退出检查。因此本地路径具备“源码检查未通过仍进入打包”的缺口；现有同版本文件的防覆盖保护仍有效，本轮没有尝试打包。

修复：先人工复核源码差异，再更新对应审阅指纹、目录和引用台账；禁止仅批量刷新 hash 消除报错。把统一源代码预检接入本地 build/release 和 CI。最终完成版额外要求验收台账冻结并通过；用户要求的中间测试包可以保留未完成验收说明，但不能跳过源码一致性与零 Go 硬门禁。

验收：改动一个受审阅 handler 后本地/CI 发行均在打包前失败；复核更新后通过；校验过程不得隐式修改清单；产物记录 source/build identity。

### B11｜高：干净 CI 缺少 ARM supervisor 构建输入

初始审查时，[CI](/Users/StatTrak/Desktop/Switch_MAC/src/.github/workflows/release.yml:30) 在 app build 前调用 [test-migration-gates.sh](/Users/StatTrak/Desktop/Switch_MAC/src/scripts/test-migration-gates.sh:19)，后者构建 EVS 后直接运行 workspace tests；[daemon build.rs](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/build.rs:14) 无条件需要 workspace `build/module-supervisor/manifest.json`。该 manifest 由后续 app 构建阶段才调用的脚本生成。准备 ARM std 的脚本默认仅离线验证，而 workflow 也未执行其显式 `--fetch` 初始化。

初始审查确认空 build 的依赖顺序缺口，开发机缓存可掩盖问题。现由共用准备入口及隔离构建复验更新此判断：i646 冻结快照以空初始编译输出完成 Release 构建和产物审计，并通过完整软件门禁；见上方 i646 证据。i648 同源码复验亦已通过，见上方 i648 证据；外部 GitHub-hosted CI 未验证，i649 同源码复验亦已通过；i650 同源码复验亦已通过；i652 当前范围元数据仍须重新冻结复验。

修复：增加唯一、可重入的构建准备入口，明确联网准备锁定依赖→校验来源/hash→构建 ARM supervisor/EVS→Rust tests/app build。离线模式只复用已验证依赖，缺少时给出明确诊断。CI、开发构建、源码归档重建共用依赖图，保持无 Go。

验收：在独立空 build 目录从纯源码和锁定依赖完成测试及 app 构建；再次运行能离线复现；缺依赖和 hash 错误必须失败且不污染已有工具链。不能删除现有唯一离线依赖来模拟干净环境。

### B12｜中：App 与后台的版本身份不一致

初始审查时 App 为 3.46/build 346，但 [Rust workspace](/Users/StatTrak/Desktop/Switch_MAC/src/rust/Cargo.toml:32) 为 3.45.0；[BACKEND_VERSION](/Users/StatTrak/Desktop/Switch_MAC/src/rust/crates/proletariusd/src/main.rs:203) 来自 `CARGO_PKG_VERSION`，运行状态接口使用该值。当时构建脚本没有覆盖这一来源。现已统一为 Rust 后端 3.46.0，并加入 source digest 与 App/backend identity 发布核验；i646、i648、i649 及 i650 冻结候选身份核对通过。该结果不覆盖 i652 后续元数据改动，当前候选需重建再核对。

修复：统一发行版本/build/source hash 注入与校验；若 crate 版本独立管理，接口显式分别命名 app_release、backend_crate_version、build_number、source_digest，不再把不同口径都称“版本”。验收 App Info、Rust 状态、日志、发行清单和实际运行二进制身份一致。此项是诊断一致性问题，不单凭版本号不同判定实际运行了旧逻辑。

## 3. “全部 Rust 后端”的明确终态

采用以下边界，避免完成接口接管后继续无限拆小任务：

| 能力 | 目标归属 | 当前需补齐 |
| --- | --- | --- |
| API、配置、持久化、短信入站/转发、设备会话、N待调度、LPA、IKE/IPsec、IMS/SIP/RTP | 单一 Rust backend | 修复上述缺陷并完成行为验收；已有原生实现不推倒重写 |
| NBLE/B0/B1、CCID/BeeSIM 协议纯逻辑 | Rust 协议模块 | Swift 中的密码派生、challenge、帧编解码、CRC、counter、认证/保活/重试决策迁入 Rust |
| 音频纯逻辑 | Rust audio core | 样本转换、重采样、DSP、缓冲/延迟和恢复策略统一；避免两种语言各维护一套行为 |
| CoreBluetooth/GATT、Keychain、CoreAudio/AVAudioEngine、权限 | macOS 薄适配层 | 允许 Swift 调系统 API；仅执行带身份/代次的命令并回报真实完成，不拥有运营商或会话业务策略 |
| SwiftUI 页面与用户交互 | Swift | 保持界面与现有功能；业务状态由 Rust 提供 |
| 成熟 EVS/AMR-WB、PC/SC/libusb 等原生依赖 | Rust 调用受控 FFI | 记录版本、许可、ABI、资源归属；不得将“Rust 后端”误称“整个程序零 C/Swift” |

蓝牙现状依据：[协议实现](/Users/StatTrak/Desktop/Switch_MAC/src/app/Sources/BluetoothReaderManager.swift:110)、[认证状态机](/Users/StatTrak/Desktop/Switch_MAC/src/app/Sources/BluetoothReaderManager.swift:1308)、[保活策略](/Users/StatTrak/Desktop/Switch_MAC/src/app/Sources/BluetoothReaderManager.swift:1416)。旧方案明确允许 Swift framing 暂留；按本次用户要求将其列入最终迁移范围，不能把它伪称 Go 残留。已在 Rust 的 B0 认证保留并复用，统一所有传输接入路径。

迁移方法：从当前成功轨迹提取脱敏协议向量→Swift/Rust 差分→测试错密码、分包、counter、迟到包、断连和重连→切换唯一 owner→同设备实机回归→删除旧纯逻辑。真实设备密码、会话密钥和供应商下发 PSK 不写入源码、日志、fixture 或报告；密码学回归使用公开或专门生成的合成测试密钥，明确标识为测试材料。删除依据是调用链已退出且替代路径验收完成，不能按目录名或语言后缀批量删。

语音策略保持 **EVS 优先，AMR-WB 后备，不启用 AMR-NB/PCMA**。当前 SDP 已遵守此策略；可用 EVS 范围是 Primary WB 13.2，DTX/AMR-WB IO 切换未开放。残留工具模块或许可证不等于当前协商使用该编码。不能为提升协商成功率宣告尚不能实际编解码的 EVS 模式，也不能保证仅支持 AMR-WB 的运营商接入会选择 EVS。

## 4. 分批实施与完成条件

| 批次 | 工作 | 退出条件 |
| --- | --- | --- |
| A：恢复可信工程基线 | B10/B11/B12；核对 131/129 口径；将本次新增 Rust 边界纳入要求 | 当前源码门禁全绿；纯源码可重建；构建身份可追溯；冻结功能×语义场景分母 |
| B：数据与操作安全 | B01–B04、B09 | 每项先有可失败的回归，再通过修复；无确认后丢片、无盲目重发、无无主任务、无暂态永久卡死 |
| C：音频正确性 | B05–B08；先保持行为修复，再提取 Rust audio core | 数值/流式/路由/代次测试通过；实际音频质量、双向播放和退出回归通过 |
| D：Rust 边界收口 | 蓝牙协议迁入 Rust，移除重复音频纯逻辑，约束薄桥 API | 协议差分与故障测试通过，B0/B1/ESTK 适用实机验收通过；Swift 无后端业务状态机 |
| E：P1–P6 综合验收 | 指定硬件业务、长稳、升级/退出/资源、最终无 Go 和构建复现 | 每个已冻结适用场景都有证据或明确用户接受的范围调整；未知项不能自动算通过 |

B 的短信/转发与 C 的音频可并行；B02–B04 必须统一 operation 生命周期设计，避免分别修补产生新的 gate/退出矛盾。D 的接口定义可提前准备，但不在 B 尚有数据或写卡风险时大范围改动同一设备链路。

C/D 的音频迁移增加实时边界验收：CoreAudio/AVAudioEngine 回调仅经有界、非阻塞 PCM/FFI 交接，不在回调里等待 Rust 后台请求或持久化。迁移前后测量队列深度、溢出/欠载、处理耗时 p95/p99、端到端播放延迟；分别覆盖模块与读卡器采样率、帧长和时间戳契约，路由变化和 stop 后旧代 PCM 不得进入新会话。

不恢复已经放弃保留的 2.80/转 Rust 前源码归档，也不再把其不存在列为无限等待条件。以当前可核实源码、可用发行物和既有证据建立新基线；缺少旧版本的行为等价证明如实标注，不虚构历史比较。

## 5. 实机与长稳验收矩阵

以下是尚需证明的项目，不全部是已确认 bug。本轮不执行付费或对外通信。

| 范围 | 必须核对 |
| --- | --- |
| ESTK + SIM World/3HK | 连接、EID/Profile、当前身份、代理路径可观测、IMS 注册、停止/退出；新版本入站短信须单独采样 |
| 普通与 N待短信 | 短信/长短信、重启恢复、多订阅隔离、通知与转发、删除/清空待确认记录；用户此前暂缓的入站实测保持待验收 |
| B0/B1 与其他支持 BLE 路径 | 密码认证、B0 BF71 加密认证、EID/Profile、启停/切换、正常授权下载兼容性及 6982 诊断均保留评估；后发用户认证要求已取代 i614/i615 的旧排除。受保护下载适用性与实际成功仍未决，不绕过认证，不把合成认证测试或错误码当成功/不适用证据。 |
| N待多 Profile/多读卡器 | 真实 readiness、旧代隔离、续期排队、切卡占用、离线恢复、每条线路实际业务；不能只看绿色“已注册” |
| 语音 | 呼入/呼出、接听/挂断/取消竞争、早期媒体、DTMF、双向音频、设备路由变化；EVS 与 AMR-WB 分别保留协商和媒体证据 |
| 模块 | USB/串口/QMI 与通话/DSP 适用路径；物理线程/句柄退出真实完成，不能由 fake transport 代替硬件结论 |
| Telegram/Bark | 系统代理与自定义代理、代理端口变化、Bark 加密/图标、重试/未知状态、真实目标确认；外部投递需明确测试目标 |
| 升级与异常恢复 | 设置和短信数据兼容、断电/写失败/进程重启、队列与日记恢复、正常退出和无法确认退出的可理解诊断 |

SIM World 按其实际 3HK 身份和当前代理配置验证。保留配置的 fake-IP/代理路径，不加直连或自建 DNS 兜底。运营商模板同步应锁定来源版本、比较真实字段语义并验证；配置相同不等于所有国家和号卡都已实测。

长稳先 24 小时，再 72 小时；IMS REGISTER 到期、EAP 重认证、IKE/CHILD SA rekey 分别统计其适用策略和实际事件。自然到期不足至少三次时延长对应观察，不能用三次 IMS 刷新替代 IKE rekey。报告总时长、掉线次数、原因、恢复 p50/p95、不可用累计时间，以及每条线路的业务证据。压缩时钟/故障注入用于软件回归，不能替代自然租期实测。

## 6. 性能优化的收益门槛与停止规则

先修正确性和完成 Rust 业务边界；这两类工作不受“省了多少 CPU 才值得修”的限制。可选性能改造沿用既定收益阈值：CPU 同时下降至少 20% 且 0.5 个百分点，或 UI 热路径改善至少 25% 且 50ms，或 90 天可估算收益覆盖至少两倍实施和维护成本。未达到则停止该项并记录取舍。

| 指标 | 固定测量方法 | 初始回归门槛 |
| --- | --- | --- |
| 启动 | 同机器 Release，冷/热各 10 次；UI、backend ready、设备 ready 分开 | p95 增加不超过 max(基线10%,100ms) |
| 资源 | 同网络/代理/Profile 数；前后台各 3×10min；统计 App+Rust+桥进程总量 | CPU 增加≤max(10%,0.2百分点)，RSS 增加≤max(10%,20MiB)；须超过测量噪声才判定 |
| 短信处理 | 脱敏 0/100/1000 条，分开记录解析、持久化、组装、UI 更新 | 本地 p95 增加≤max(10%,20ms)，网络延时单列 |
| 恢复 | 显式断链到撤销就绪、重新注册、首次业务成功分别计时 | 显式断链撤销就绪 p95≤3s；黑洞按实际心跳预算，恢复不退化超过10%或噪声 |
| 长稳 | 24/72h 的 RSS、任务数、FD、队列长度、恢复后回落 | 无持续未回收增长、无旧代任务继续写状态 |
| 构建与磁盘 | 冷/增量各3次；分别统计源码、离线依赖、编译缓存和发行物 | 零 Go 为硬指标；缓存按精确清单清理，保留必要离线依赖与证据 |

不为降低行数或消除所有历史命名而大规模改写 `main.rs` 等文件；在修复涉及的边界逐步提取有明确接口的组件。先测出分配/锁竞争/轮询热点，再决定是否做缓冲复用、批处理或事件驱动；不预设 Rust 必然更快。

任一批次出现串 Profile、重复写卡/发信、确认后丢数据、旧会话资源提前复用、音频明显退化或代理行为改变，停止该能力切换并回到已验证路径；保留证据，其他独立批次可继续。不能用强杀、盲目重试或缩减测试分母获得“完成”。

## 7. 项目进度与下一步

当前阶段仍为：**P0 需求范围与实例未冻结；P1–P4 有 i650 同源码软件证据但实体/运营商业务验收未闭合；P5 双向实时音质与性能指标未闭合；P6 外部 CI、旧版升级回退、长稳与最终交叉审议未完成。** 最近完整软件门禁、冷构建和身份核验属于 i650 digest `62f65cd6afb6…`（Rust workspace 3,974/0/9，Swift/BLE 合成回归，协议回放 297 次）。i652 仅落实后发认证要求：B0/B1 密码、B0 BF71、正常授权下载兼容性及 6982 诊断仍在范围内且未决，旧 i614/i615 排除已被取代；受保护下载实际适用性待合法能力证据，不绕过认证、不宣称成功。实体 DJI、SIM World 短信/通话及长稳均不能由这些软件结果推导通过。

可单独报告的接口归属为 **100%（294/294，含54 HEAD）**；i650 冻结快照的软件门禁及身份已核对。i652 按后发用户指令修订现有 NINE_ESIM 范围与来源版本，保留 i614/i615、i649/i650 历史记录，不新增叶子、实例或功能 PASS。整体分母仍未冻结，接口数、测试数和元数据检查不代表项目完成百分比。

i652 当前范围和来源锚点冻结后，由主任务执行同摘要完整门禁与构建身份复核。随后逐项收口 P0–P6 的合法适用范围、实体卡与运营商、6982 诊断、SMS、双向语音、24/72 小时长稳和外部 CI。受保护下载的授权与能力未核实前保持未决；不绕过认证、不把错误返回改成成功。全部代码和功能验收闭合后才进行三轮顺序独立审议、修正复测及最终发行；本轮不生成 DMG。
