# Iteration 577：固定 ADB 目标与冻结恢复输入

日期：2026-10-19。整体仍为 **P1–P4 同步验收、P5 完善、P6 最终验收未完成**。接口归属为 294/294 Rust；完整必需验收目录的子条件和适用矩阵尚未冻结，不据此估算整体百分比。本轮修复共享模块 authority 的两个前置缺口，没有完成共享 authority 或正常模块恢复验收。

## 本轮生产改动

ADB 的一次计划现在持有首次选定的原生 `rusb::Device`、接口、alternate 及端点。所有候选完成检查后，只有唯一支持设备才允许打开；多个候选或描述符不能可靠确认时拒绝，不尝试下一个设备。准备、部署、启动、状态、停止和回退在同一计划内使用同一目标，不再每条 shell 命令重新选择第一台可打开设备。

只有原生 `NoDevice` 被映射为目标断开并锁存。普通协议、认证、超时、claim 或 I/O 错误仍允许显式停止或回退在原目标上重新连接，不自动重放业务，也不切换到替代设备。8 项新增测试使用生产计划、会话及协议处理配合假目标；原生枚举、真实拔插及 AT→ADB 物理设备配对尚未验证。该目标只覆盖同一计划，不能代替跨请求的共享设备身份。

配置恢复在接纳前捕获备份的根目录、备份目录及文件身份、原始 SHA-256 和解析结果。路径使用不跟随符号链接的相对目录打开，文件必须为有界、单链接普通文件；读取前后核对元数据。进入 radio reservation 前及 worker 恢复前再次核对身份和原始摘要；文件内容改变、同字节替换、符号/硬链接或目录替换均不能让新内容被悄悄接纳。worker 只消费已捕获的配置，保留设备身份、空闲检查、安全备份、重启重绑、回读及释放规则。`last_restore` 另加 `backup_sha256`，原字段保留。

交叉审议同时发现旧 schema 2 备份若额外带有 `voice`，会被旧转换路径误用于 IMS 恢复。现在 schema 2 的这类字段不进入恢复目标，摘要也不宣称包含语音配置；原始文件及导出字节保留，schema 3 原有校验继续生效。新增 5 项测试覆盖捕获兼容性、摘要、替换/链接/目录变化、schema 2 附加语音，以及接纳前拒绝变化且未预留 radio。尚未运行真实模块的成功恢复，未将源码检查或接纳前测试算作完整 worker 恢复及回执验收。

## 实际验证

| 证据 | 结果及适用范围 |
| --- | --- |
| [完整 daemon 测试](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-daemon-full-result.json) | 当前 daemon 全量 2,555 passed、0 failed、0 ignored，较 576 新增 13 项。在强制拒绝硬件和外网的沙箱中运行，canary、实际进程组退出及源码零漂移通过。其他工作区测试程序未在本轮重跑，不能称作本轮 43 程序全通过。 |
| [契约与 Go 退出检查](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-default-gates-result.json) | 294 个 Rust 接口、54 个派生 HEAD、432 个审阅范围，Swift 为 129 个 HTTP 与 2 个非 HTTP 调用；生产默认构建/运行 Go 退出检查通过。历史旧二进制比较工具另行保留。 |
| [Release 后端构建及 GUI 对照](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-release-backend-build-v2-result.json) | 运行实际生产 Release 后端构建入口；Swift 和资源没有改变，复用 576 GUI。未声称重新执行完整 Swift/Xcode 编译。更改候选 Bundle ID 后签名会改变 Mach-O 全文件摘要，首次错误的全文件相等检查保留为失败；仅在临时副本去除签名后，GUI 代码载荷摘要一致。 |
| [候选应用检查](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-candidate-app-verification.json) | arm64、严格 ad-hoc 签名、许可、包内仅 Rust 后端及 libusb、无原生故障注入符号均通过。未安装、未公证。 |
| [包内生命周期](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-daemon-lifetime.json) | 实际包内 daemon 的 17 个场景、23 个进程通过。 |
| [日志退出](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-candidate-backend-process.json) | 三种日志管道退出通过。 |
| [正常 archive 编码向量](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-native-release-vectors.json) | 最终正常 EVS archive 的 10 种模式向量对照通过；没有重计历史 150 个分配失败点。 |

冻结输入仍为 2,891 项，较 576 改变 8 项：6 个 Rust 文件、2 个 API 审查元数据。人工复核更新三个发生变化的生产作用域。编译测试完成后删除测试 target，不影响保留的日志、指纹和当前候选；576 尚待通过的精确 API handoff 程序仍单独保留。

真实跨语言成功落盘前置 `tests::swift_forwarding_handoff_fixture_round_trips` 仍未完成；本轮 daemon 没有 ignored 不等于该工作区用例已通过。576 正常 PAC 配置失败及独立平台专项通过的历史结果也不由本轮覆盖。

## 缓存和产物

清理本轮 `build/rust-target`、空 `build/swift-cache` 两项：分配空间 1,365,397,504 字节，同期可用磁盘增加 1,298,276,352 字节。前后 2,891 项输入及 11,561 个保留条目无漂移，见[编译缓存清理](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-cache-cleanup.json)。

另外按逐文件身份与摘要清理两个工作区内旧 Go cache 中的 633 个生成条目，分配空间 104,681,472 字节，该独立区间可用磁盘增加 105,234,432 字节；四个历史 `scan_go` 工具及四个说明/元数据文件保留，外部缓存链接未跟随。见[旧缓存清理](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-legacy-go-cache-cleanup.json)。两次清理合计移除约 1.47 GB 分配空间；磁盘可用变化是观察值，不能保证与分配值一致。

候选为 `build/iteration-577-derived/Build/Products/Release/Switch.app`，ID 为 `com.proletarius.switch.candidate577`，Rust 后端 SHA-256 为 `f7c0fe52fd87783f07b24060ec5fd41d1674a10cd499ddd0ccc92096490ef467`。已安装应用仍为 3.43/343，后端摘要保持 `29cd95a16a905afc095ea55444caeb59aa7b9f13f7637cedcdf1bad4932f7933`；正式 dist 未替换，无 DMG。本轮未停止已恢复应用、读取其数据或操作实卡，没有短信、拨号、切卡及下载。最近实卡证据仍是 574 的 ESTK 在位但 MUTE、ATR 长度 0，不能当作本轮重新检测。

## 下一步与明确未闭合项

共享模块 authority 还须统一新业务准入、已接纳操作的结算/清理权和恢复专用准入，并在所有克隆中单调撤销新准入。单独动态化当前 Gate 会阻断原有释放和退出，不能作为完整修复。

后续需要在 AT USB 连接丢弃前捕获原生物理身份、为显式串口验证 IOSerialBSDClient→USB 映射，并使跨请求的 radio/USB/ADB 义务绑定同一设备；不能把本轮 ADB 计划当作这条链已完成。启动隔离状态下的合法备份恢复仍缺独立 RecoveryAdmission，本轮未放开全局 Gate。

持久 pristine 或新 journal 不能证明未参与协议的旧程序没有运行；受控 legacy 生命周期、旧 3.42 退出 1、原位升级回退、真实 handoff、当前候选硬件/运营商业务、干净环境、性能及自然长稳验收继续保留。完整结果入口为[577 门禁摘要](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-577-gate-summary.json)，整体目标保持进行中。
