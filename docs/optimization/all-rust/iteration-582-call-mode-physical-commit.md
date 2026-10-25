# Iteration 582：通话模式配置的物理写入权限

日期：2026-10-19。P1–P4同步验收、P5完善中、P6未完成。接口接管100%（294/294）单独计量，不表示整体产品完成。

## 生产改动与范围

普通 call-mode 配置现在使用不可由请求参数构造的 `CallModeCommitProof`。证明由正在执行的 radio owner 根据真实 pending attempt 创建，封存动作及目标、producer/epoch、原 reservation、操作锁、lifetime RootProof 和有限 operation right。弱引用不保活原授权，失败会使共享证明失效。不同实际 actor 即使使用相同 session 数字，也不能复用证明；实际接纳处另核对自身 Gate。

证明贯穿 USB/serial 排队和 Plan，配置的每次实际写入前重新验证。缺少证明的 typed voice、USB composition、ADB 持久授权配置不能直接写入。既有只读、RF工作和已经接纳的有限清理保持其原有权限；正常调用仍从实际 owner 获得合法证明，没有通过全量禁用原功能来制造通过。

USB 的最终检查位于旧响应 drain、deadline 和 prompt 等待之后、实际 `Io::write` 之前。交叉审议发现 proof 自身不能替代 USB owner 同步撤代，因此最终回调同时复核原 Inner、epoch、producer、session 和 inhibit。串口在真实 `write_all` 和 submission 标记前检查。metadata/state 检查与随后内核 I/O 不是原子操作，不宣称防止该间隙任意并发变化，也不等同于跨重枚举的实体身份证明。

本步尚未发送字节时拒绝不新增 submitted；先前已经发送、短写或可能提交的 unknown 不清除、不自动重放。原资源的 release/close 和已接受义务仍由原 owner 结算。ADB 命令及错误的敏感内容在拒绝分支清除，实际发送 payload 使用 Zeroizing；这不等于所有临时响应或分配器内存均已证明擦除。已经可能进入编辑器的操作只尝试一次有预算的原通道 ESC，不能借清理重新发送正文或开启新业务。

## 实际回归及失败记录

冻结2905项输入，比581变化17项，无删除；最终 manifest SHA256为 `2a8703d3f5b469614c94b6102bbfce1bfd2cc5745352b3ae70b9002c6e52f7c2`。首次编译失败于 include 文件内的 inner doc comment 和 owned message 转换，修复后重新冻结 final-v2，保留初次失败、精确差异和独立复核。生产 proof 错误保留原 status/code/message，空 response、uncertain=false，仅用于物理分发前的失败；没有放宽验证。一次过早启动的 census 在审议指纹更新完成前读取旧值而失败，随后顺序执行及最终门禁通过，失败没有改记成功。

v2首次完整daemon为2715通过、11失败，原始结果保留。8项USB测试的ACK脚本使用缺少前导CRLF的裸OK，真实收集器未将其识别为结束而继续读取；仅修正三处响应帧。另3项旧radio测试以短只读预约模拟持久配置操作，该预约没有原operation lock；改为与生产通话模式修改一致的exclusive预约，并保留动作/幂等断言和补充短预约拒绝验证。这是明确的安全契约修正：短只读预约不再授予持久配置写权限；生产修改入口已有独占预约。v3测试修正没有放宽生产校验，最终源码与v2生产指纹一致。

最终完整 daemon **2727项通过、0失败、0忽略**，与581完整日志逐项比较新增 **34项**，无旧用例删除。新增回归使用真实 Lease/Gate、实际 owner mint、真实 mailbox/spawn 收据、串口 PTY及 USB NativeTransport 的受控私有 Io。覆盖有效提交、排队/读后/最终 drain 中的失效、不同实际 actor、prior unknown、敏感授权、有限 ESC 和原资源结算；没有伪造 libusb Device，不替代实卡。

API常规9项通过，所需 ignored fixture 在独立持久化链中精确执行1项通过。三个独立进程集成程序合计10项通过。复用经全部 app 源码及原二进制指纹核对未变的581 Swift工具，再执行17项 checkpoint、完整客户端及两类真实持久化失败重试。这里没有重新编译全部 Swift，也不是完整 SwiftUI 测试矩阵。

API/Go退出门禁通过：294个Rust owner、0个Go owner、54个HEAD别名、438个审议scope、129个Swift HTTP及2个非HTTP调用。没有调用Go。

## 候选构建及跨进程验证

通过生产构建脚本新编译 Release Rust后端，复用逐项源码验证未变的581 GUI和资源。原GUI与候选GUI的去签名副本载荷一致；候选重新严格ad-hoc签名，arm64、许可证及仅Rust/libusb后端检查通过，包中无测试故障注入符号。候选ID为 `com.proletarius.switch.candidate582`，后端SHA256为 `664d12e355f538fe9ac9ef468e104c755c4c38044805e5864595cab327db5c1a`。这不是一次全新SwiftUI编译，未安装候选、未替换正式dist、未生成DMG。

实际包内17场景23进程、三类日志管道正常退出和当前EVS archive的10模式向量通过。真实跨语言持久化链完成3场景8进程：独立Swift落盘/重启、精确Rust DTO、第三Swift核对，以及实际包内daemon重启、token重放/401和两类409冲突；全部子进程正常退出回收，无强制清理。普通API套件中的 ignored 不单独计通过。

## 精确缓存清理

所有构建和验证进程结束后，仅按Ruby cleaner列出的两条精确路径 `build/rust-target` 和空的 `build/swift-cache` 清理，lsof确认无打开文件。回收分配空间 `1685725184` 字节，同期可用空间增加 `1473957888` 字节；2905项输入及 `17389` 项保留条目零漂移。空Swift目录由构建环境创建，本轮没有新Swift编译。报告、baseline、runtime、EVS/archive/向量、候选Products、离线依赖、安装应用及正式dist保留，没有删除整棵build/tmp/runtime。

## 剩余项目目标

本轮只关闭普通 typed call-mode 配置在既有 actor/会话中的物理提交证明缺口。完整 Startup RecoveryCoordinator、独立恢复owner、有限恢复计划及耐久journal、完整held-FD启动证据、legacy共享guardian、精确实体重连/无AT/串口/重枚举仍需独立实施和验收。普通proof不能换取启动恢复权限。

本轮没有操作读卡器、切卡、下载、真实短信或拨号。580的ESTK MUTE/ATR0及身份读取失败仍是历史失败，不能当作582硬件结果；用户重新插拔条件仍待确认。当前候选的真实运营商短信/USSD/通话、Telegram/Bark实际投递、多Profile/设备/运营商矩阵、自然刷新/rekey/reauth、24h/72h长稳、升级回退与整机性能继续保留。49个父门尚不是冻结的验收分母，不以测试数估算整体百分比。项目目标保持active。

证据：[最终摘要](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-582-gate-summary.json)、[完整回归](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-582-final-v3-daemon-full-result.json)、[新增用例](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-582-new-test-evidence.json)、[跨语言链](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-582-handoff-durability.json)、[缓存清理](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-582-cache-cleanup.json)、[后续门禁](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-582-next-gates-review.md)。这些软件通过项不代表P0–P6或实机全部验收完成。
