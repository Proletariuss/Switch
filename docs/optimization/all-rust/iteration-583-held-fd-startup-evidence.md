# Iteration 583：启动证据绑定原目录描述符

日期：2026-10-19。接口接管100%（294/294）单独计量；P1–P4同步验收、P5完善中、P6未完成。

## 生产改动

真实 daemon 启动检查现在在 `RootProof::with_root` 内直接读取原 Lease 持有的目录描述符，不再忽略该描述符并重新按根路径读取。待恢复标记、pristine 标记、两类旧锁、备份目录和语音运行时的存在及形状均使用相对原目录的 nofollow 元数据。

语音运行时检查固定同一 runtime/version 目录及每个实际文件句柄，在同一文件上核对类型、链接数、完整权限、长度和有界 SHA-256，并在捕获、读取、目录枚举及最终验证边界复核原文件和名字。目录清单必须与当前 manifest 精确一致；不允许未知版本、额外文件、链接、特殊文件或不完整下载被认作完成。

新增小型 Rust Unix 系统桥 `proletarius-platform-fs`，仅公开有界目录名字枚举。它通过相对原目录打开 `.` 获取独立游标，保留调用方句柄；`fdopendir` 成功后才移交所有权，失败时实际关闭仍拥有的描述符。`readdir` 区分 EOF 与错误，限制记录与名字复制，保留非UTF8字节，拒绝重复、超额及未在预算内结束的枚举，检查实际关闭结果。必要的系统 FFI 隔离在该 crate，主 daemon 的 `forbid(unsafe_code)` 保持不变，没有新增下载依赖或生产故障注入开关。

根目录及两层运行时目录的身份、权限、链接数、mtime/ctime 等完整印记只在本次扫描内复核；它们不会变成长期代次标志，后续正常创建 store 不会误触发 live 撤权。实际名字替换或可见变更保守失败；即使名字被临时换回，读取始终使用原句柄，不能混入另一棵树的文件。

这里没有跨系统原子快照保证：任意不可观察的 ABA、最终检查后的变化及底层文件系统调用的硬超时不在本轮证明内。有界表示读取字节数、名字数和调用次数受限。原有 pristine/锁/manifest 格式及静态 reason 顺序保留；普通无 RootProof 的 path convenience 现在拒绝根 symlink，真实 Lease 的 requested alias 经校验后仍兼容。

## 已验证的软件范围

冻结2909项输入，比582变化10项，新增4个文件，无删除；manifest SHA256为 `beb6af6a931a3680fba9710dd99d82744a9c180ac88b531cf4d05c4b7b40706f`。最终 daemon **2740项通过、0失败、0忽略**，与582完整日志比较新增 **13项**，无旧用例删除。独立系统桥 **8项**测试通过，组成是7项真实文件系统测试及1项实际生产字节decoder测试；真实场景包括255字节名字、独立游标、上限和 fdopendir 失败关闭。

新增 daemon 测试覆盖合法旧格式、坏文件形状、reason 顺序、原根/子目录替换、可观察 ABA、同inode同长度内容变化、最终 inventory 复核、真实 alias 与 Lease 退出，以及启动后正常写 store 不误撤权。其中一个函数为 FIFO 子进程夹具：父用例分别创建真实 FIFO 和打开后替换场景，按精确用例启动，要求自然退出及完成收据；超时或强杀不能算通过。这些是实际文件和原生产扫描器的测试，不是伪造 verifier 结果或真实设备恢复。

初次编译通过，首轮定向13项为11通过、2失败，原报告和日志保留。version被替换时父目录inventory已更早拒绝，原测试错误地要求仍然hash一次；现分别精确要求version替换0次、runtime替换原树1次，两者仍拒绝。另一项尝试创建非法UTF8文件名，沙箱内返回EPERM；独立沙箱外真实探测三个名字均被工作区文件系统以EILSEQ92拒绝。改为实际Unicode额外名的原生allowlist测试，非法字节的无损/首NUL/非空规则从原生产逻辑等价提取为私有decoder，由真实读取函数调用并独立验证。没有伪造DIR或放宽扫描；本机非法UTF8文件系统组合仍未验证，不把decoder测试当作其端到端通过。

API常规9项及独立持久化链中的专项1项、10项独立进程集成、17项 Swift checkpoint、完整客户端及两类真实持久化失败重试通过。API门禁为294个Rust owner、0个Go owner、54个HEAD别名、441个审议scope、129个Swift HTTP及2个非HTTP调用。新增三个完整生产scope；不能把此前未纳入scope的module_recovery说成此前已有API指纹。

初次全平台离线依赖元数据解析因未缓存的 `core_detect` 失败，原始失败保留，没有联网补包。当前 aarch64-apple-darwin 的锁定离线解析及构建通过；锁文件仅增加内部 crate 和 daemon 依赖，所有既有外部包及版本逐字节不变。一个审议scope总数预检错误地假定440，核对后确认原module_recovery没有既存scope，最终为441；失败发生在写入前，没有放宽检查或修改结果。未把这些前置失败当成已通过测试。

## 候选与进程验证

通过生产脚本新编译 Release Rust后端，复用已核对全部 app/data 源码和原载荷未变的581 GUI及 Swift工具，没有新编译GUI。候选严格ad-hoc签名、arm64、许可证、仅Rust/libusb后端及原生/Swift测试故障入口检查通过。候选ID `com.proletarius.switch.candidate583`，后端SHA256为 `c7eb2e90be591a535e97ed25a2b85240e21d56c8edba37f5a4b03a6ea8ae1e67`。

实际包内17场景23进程、三类日志管道退出、真实跨语言3场景8进程持久化链以及当前EVS archive的10模式向量通过。所需 ignored API fixture 已在独立链精确执行，不能只依据普通套件中的 ignored 计通过。所有验证子进程正常结束回收；未安装候选、未替换正式dist、未生成DMG。

## 清理与后续目标

全部编译、测试与子进程结束后，只清理Ruby cleaner列出的 `build/rust-target` 和空 `build/swift-cache`，逐项lsof确认没有占用。回收分配空间 `1688010752` 字节，同期可用空间增加 `1492635648` 字节；2909项输入及 `18671` 项保留条目零漂移。源码、离线依赖、基线、报告、runtime、EVS/archive/向量、候选Products和安装/正式产物保留。

本轮完整关闭启动检查的 held-FD 证据前置工作包，仍未实现完整 Startup RecoveryCoordinator。独立恢复owner/right、耐久journal、精确USB bootstrap、重启后实体后继证明、有限恢复步骤与实际清理仍须接通；当前恢复入口保持原隔离，文件证明不授予恢复权或解封普通Gate。具体接线已核对并记录，不能用普通 OperationRight 转换或全局重新枚举冒充恢复。

本轮无实卡、切卡、下载、真实短信或拨号。当前候选的实网短信/USSD/通话、Telegram/Bark投递、多Profile/读卡器/运营商矩阵、自然IMS/IKE/CHILD期限、长稳、legacy共享生命周期、升级回退和整机性能继续待验收。580的ESTK MUTE/ATR0失败保持原记录。49个父门没有冻结为整体分母，不从接口或测试数量估算项目百分比；项目目标保持active。

证据：[最终门禁](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-583-gate-summary.json)、[完整daemon](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-583-final-v2-daemon-full-result.json)、[系统桥](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-583-final-v2-platform-fs-full-result.json)、[交叉审议](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-583-fd-evidence-cross-review.md)、[真实恢复接线](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-583-startup-recovery-wiring.md)、[缓存清理](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-583-cache-cleanup.json)。
