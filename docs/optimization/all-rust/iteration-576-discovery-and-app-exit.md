# Iteration 576：读卡器自动选择与应用退出

日期：2026-10-19。本文记录本轮已取得的软件证据及未关闭事项，保留 iteration 575 的历史结果。当前仍为 **P0 需求映射待冻结、P1–P4 同步验收、P5 完善、P6 未完成**；294/294 仅表示接口归属覆盖，不表示整体产品完成。编译与测试使用冻结的 2,891 项源码输入，本说明文档不在该编译输入集合内。

## 生产行为

- `/api/reader/list` 增加可选 `devices`，旧响应仍可解码；它与聚合快照共用同一次枚举结果的卡片字段和 ATR 十六进制投影，没有新增扫描或伪造 discovery 路由。
- 聚合与列表回退统一自动选择条件：候选必须属于同一响应的 `readers`，名称无重复，且观测为 `present`、非 MUTE。缺少卡片观测、未知、缺卡或冲突时不再退回 `readers.first`。该条件是 OS 准入提示，不保证 APDU、鉴权或注册成功；手动 BLE 连接仍可使用。
- 枚举未确认本地已有选择时，先撤回展示身份及陈旧 readiness，再独立核实 retained owner。未知或失败不自动换卡；只有明确的逻辑断开才允许依据已有卡片观测重新自动选择。私有已验证身份锚点及正在进行的 stop 所有权不因临时未知被丢弃。
- 列表失败时，已有 owner 可独立走 info→status 恢复链，不等待模块健康读取；合成的名字响应不携带可自动选择的卡片证据。该链路的异步完成路径均核对 generation、请求及 operation/status revision。stop 尚未完成时不发起竞争状态读取；info 已成功核实后，后续 status 失败仅撤回 readiness，保留新身份及 ePDG。
- 正常应用退出进入 `stopping`，等待确切受管子进程的实际退出，再停止 BLE 桥并回复 AppKit 退出请求。退出意图阻止再次 start；普通 stop 则在实际退出后仍允许重启。等待超时、取消、重复退出及迟到回调不能伪造退出回执。软件夹具通过不等同于真实 NSApp、硬件清理或跨版本恢复验收。

## 已核实的软件证据

| 证据 | 本轮结果与边界 |
| --- | --- |
| [Rust 工作区运行报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-rust-workspace-result.json) | 实际执行 43 个测试程序：3,370 passed、1 failed、1 required ignored；其中 daemon 为 2,542 passed、0 failed、0 ignored。正常配置整体仍为失败，未改写为全通过。 |
| [PAC 定向平台报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-pac-platform-result.json) | 相同 PAC 测试程序在单独平台配置下 6 项通过。与正常配置去重后共 3,371 项有通过证据；正常配置的 PAC 失败继续保留。该控制仅覆盖 literal PAC，不证明真实代理链或硬件业务。 |
| [Swift reader 报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-swift-reader-result.json) | 完整入口通过，含原有身份/代次/迟到 GET 围栏，以及双入口自动选择、owner 核实、列表失败独立恢复、模块挂起、stop、未知观测恢复、503、手动 BLE 和状态失败保留新身份场景。 |
| [Swift menu 报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-swift-menu-result.json) | 隔离夹具中的完整 suite 通过；不据此声称已安装应用的真实 UI 验收完成。 |
| [BackendReadiness 首次报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-swift-backend-result.json)、[v2 报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-swift-backend-v2-result.json) | 首次测试因自身读取 `/dev/fd` 被强隔离拒绝而失败。测试改用 `fcntl` 统计自身 FD 后，v2 完整运行 13.141 秒通过；资源断言未降低，首次失败报告及日志保留。 |

Rust 的 required ignored 为 `tests::swift_forwarding_handoff_fixture_round_trips`，仍需其文档化前置条件及精确用例验证，不能计作通过。软件运行的源码前后校验及子进程退出信息以各报告为准。

旧 reader 源码负对照已成功编译，并在强隔离下触发精确预期断言：`aggregate auto-selection requires unambiguous same-response card evidence`。原驱动因匹配了转述字符串，将 [原报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-swift-reader-old-control-result.json) 标为 false；[独立 assessment](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-reader-negative-control-assessment.json) 核对原日志后确认该预期失败，无重跑或覆盖原失败。它证明旧实现可触发新回归检查，不表示旧 suite 通过。

## P0 与 P6 的未关闭事项

[需求审计](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-requirements-audit.json) 已提供 49 项候选门及缺口映射，但完整必需子条件、适用矩阵与证据规则尚未冻结，不能据此给整体百分比。显式调用旧 Go 二进制的回退验证工具属于历史基线/比较工具，不是生产 Go 回退；其最终交付分类与退役边界仍需明确，也不能声称所有工具均已不依赖 Go。

[模块 authority 审计](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-module-authority-audit.json) 保留三类未关闭问题：持久化 pristine 不能证明未参与协议的旧程序未运行；启动时 Gate 不能形成共享的实时撤权；合法备份恢复尚缺恢复专用准入。单独把 Gate 改为动态状态可能阻断已有清理、恢复及退出，因此本轮未实施局部动态 Gate 修补。共享 authority、精确清理回执和正常模块恢复须作为完整方案继续验收。

真实成功持久化 handoff、正常业务/硬件覆盖、干净环境、共享升级/回滚、性能与自然时长观察仍各自保留，不由本轮软件通过数替代。

## 本次记录截止状态

独立 Release 576 候选完整构建、arm64、本机 ad-hoc 严格签名及许可检查通过；backend 目录仅包含 `switchd-rs` 和 libusb，没有 Go 载荷或原生故障注入符号。包内实际 daemon 的 17 个场景、23 个进程及三种日志管道退出通过。正常 EVS archive 重建后指纹改变，已重新执行 10 种模式向量对照并通过；没有把历史 150 点分配故障测试算作本轮新增。这些结果不等于公证、新机器干净构建或实网业务验收。

2,891 项输入冻结，较 575 改变 18 项：13 个 Swift、3 个 Rust、2 个审查元数据。人工复核更新四个发生变化的已审查作用域；当前 432 个作用域、294 个接口（含 54 个派生 HEAD）均通过一致性检查。Swift 调用清单为 129 个 HTTP、2 个非 HTTP。默认生产 Go 退出检查通过；第一次驱动误传不支持的 `--require-clear` 参数，原失败保留，使用既有 `--require-go-free` 后通过，没有改变检查器或放宽条件。需求审计所载旧调用数量是审计时点的 575 输入，当前数量以最终 576 inventory 为准。

所有构建与测试结束后，精确清理 7 项编译缓存，分配空间共 2,204,139,520 字节；同期可用磁盘增加 1,982,328,832 字节。清理前后 2,891 项源码及 10,584 个保留材料条目无漂移；Products、离线依赖、EVS、运行夹具、报告和回退基线保留。未通过的精确 API handoff 测试程序已另存，不能因 target 缓存删除而丢失后续验收入口。见[缓存清理报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-cache-cleanup.json)。

候选位于 `build/iteration-576-derived/Build/Products/Release/Switch.app`，bundle ID 为 `com.proletarius.switch.candidate576`，包内后端 SHA256 为 `cf13d7dd8b29613b959cc9976eb4aa8bc9d118d5cb072c8d69fa01b3fac74133`。没有安装此候选或替换正式 dist；已安装应用仍为 3.43/343，后端指纹保持 `29cd95a16a905afc095ea55444caeb59aa7b9f13f7637cedcdf1bad4932f7933`。用户授权的旧后台强退及恢复已在 574 完成，本轮没有重复停止恢复后的应用，没有新增实卡、短信、电话、切卡或下载操作，未生成 DMG。最近一次实卡观测仍仅是 574 的在位但 MUTE、ATR 长度 0，不能将其说成本轮重新检测结果。

最终证据入口为[576 门禁摘要](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-576-gate-summary.json)。下一工作包优先落实共享模块 authority：分别约束新业务准入、已接纳操作清理权与显式恢复权，不能用单个动态布尔 Gate 阻断已有退出；随后继续真实 handoff、当前候选硬件/运营商与长稳验收。整体目标保持进行中。
