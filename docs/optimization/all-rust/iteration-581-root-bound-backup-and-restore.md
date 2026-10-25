# Iteration 581：恢复备份、目录归属与还原回执

日期：2026-10-19。P1–P4同步验收、P5完善中、P6未完成。接口接管100%（294/294）单独计量，不表示整体产品完成。

## 本轮生产改动

实际 daemon lifetime Lease 现在提供不可伪造的弱 RootProof。proof clone不保活授权；原Lease退出、根目录或lifetime锁被替换、权限变为不安全时，不能继续使用旧证明。备份创建和捕获必须传入真实proof，archive worker使用Lease实际持有的目录描述符；接收还原的生命周期再次核对同一authority，不能用同路径重新取得的Lease或另一目录的合法备份代替。

operation/ADB interface锁统一使用保留的非阻塞内核guard。固定root与锁inode，nofollow/nonblock、打开前后及加锁后检查类型、同用户、空普通文件、单硬链接和安全权限；兼容正常旧0600/0644锁及0700/0755目录。保留已有证据，不chmod、truncate或unlink旧锁。真正锁竞争仍返回busy；不安全形状或IO错误使用unavailable。新配置入队及最后还原回执检查原guard，已接纳的Stop、settlement与release仍能结算原资源。

保护备份通过固定描述符创建：独占0600临时文件、完整write_all、真实文件fsync、linkat原子不覆盖提交、仅删除本次原inode的临时名、备份目录及root同步，再真实同步文件并读回核对原payload。任何已有目标都不覆盖；提交后同步或命名空间不确定返回Storage并保留证据。若在link与临时名清除间崩溃，可能保留双硬链接，不能直接作为正常可恢复文件；显式恢复仍待实现。旧内部创建入口保留JSON/schema/ID与错误类别，不能据此取得设备操作权。

持久配置已经一致的普通还原分支，现在先排空本进程真实语音义务；没有义务或只有prepared状态时不新建ADB探测。实际Stop/音频rollback与USB释放成功才结算原记录；取消HTTP等待者不能取消实际cleanup owner。清理后旧AT/session无法继续核实时保守失败，不用通用重连冒充原连接。

最后成功回执不再依赖可包含错误的只读状态投影。optional readiness检查后，还必须真实typed读取模块身份、完整USB与语音配置，核对所选备份，再复核原备份/root/operation lock。错IMEI、USB漂移、读取失败/不完整或voice漂移均不能发布last_restore。原始配置可以关闭ADB/UAC，不要求state等于ready；schema2继续只恢复/比较其包含的USB目标，不采用附加voice字段或进行额外写入。

## 保留的失败与修正

首次完整daemon结果2688通过、4失败，完整原始记录保留。三个权限夹具错误地把chmod请求值当成实际值；同一强OS沙箱下独立观测证实2755/4755实际为0755，2600/4600实际为0600。修正后先明确核对实际mode，再分别完整验证合法接受或不安全拒绝，并保留元数据、零work/恰一次work断言；没有skip。新增真实safe_mode函数的所有特殊位与共享写位策略测试。另一个socket夹具路径超过Darwin SUN_LEN，改用短workspace路径并先确认实际socket建立。

修正只涉及三个测试输入；production prefix逐字节相同，完整production tokens相同。v2冻结驱动初次错误假定两个authority文件已有API review scope，第二次错误比较了保留测试空白的production eraser文本；均未改生产或生成成功结果，随后改为核对真实冻结前baseline、生产前缀及全部production token。原始错误说明保留。

## 已通过的软件门

最终冻结2901项输入，比580变化17项（新增5个文件），无删除。完整daemon2693通过、0失败、0忽略；API常规9通过，所需ignored用例已在独立持久化链中精确执行1项通过，三个独立进程集成程序合计10项通过。17项Swift原始checkpoint、完整客户端和两类真实持久化失败重试通过；这些是对应受控程序验证，不是完整SwiftUI测试矩阵或真实用户原位迁移。

生产Go退出与API门禁通过：294个Rust owner、0个Go owner、54个HEAD别名、437个route/consumer审议scope、129个Swift HTTP及2个非HTTP调用。两个既有root authority模块另有完整生产指纹交叉审议，不能谎称它们此前已在API scope列表。

## 本轮未关闭的边界

RootProof和备份文件证明不等于设备操作权。normal lifecycle/入队及完成边界的检查，尚不能替代USB/serial每个实际物理write点的证明；queued及多步read→write之间的namespace变化、未知提交和cleanup区别已单独审议，留待下一工作包。startup证据检查仍是proof前后包围的路径检查，不宣称原子FD快照。完整Startup RecoveryCoordinator、有限typed恢复计划、legacy共享guardian、无AT/serial/重枚举及精确AT重开仍未完成。

本轮没有接触读卡器、切卡、下载、发真实短信或拨号；580的ESTK MUTE/ATR0及身份读取失败仍是历史事实，不能当作581硬件验证。用户重新插拔目标读卡器/SIM World后才进入后续实机读取、注册及被动收信验收。多Profile/设备/运营商矩阵、provider实际投递、自然期限长稳、升级回退与整机性能按原计划保留。整体目标保持active，不生成DMG，不替换安装或正式dist。

## 实际候选与跨进程验证

完成全新SwiftUI/Rust Release构建（未复用旧GUI），严格ad-hoc签名、arm64、许可与仅Rust/libusb后端检查通过；生产包无原生codec或Swift checkpoint故障注入入口。候选ID为`com.proletarius.switch.candidate581`，后端SHA256为`e0c37f7489879a609721114180781682ba98d898a45bb8ccfa5a75f82025746d`。实际包内17场景23进程、三类日志管道正常退出，以及当前正常EVS archive的10模式向量通过。

真实跨语言链执行3场景8进程：独立Swift写入并重启读取、精确Rust DTO、第三Swift核对原文件；另用当前包内Rust daemon实际GET/POST、双方重启、新token重放、旧token401及两类409冲突核对原ledger不变。所需ignored用例已真实执行成功，不能仅依据普通API套件中的ignored计通过。各子进程正常退出/回收，无强制清理。

## 精确缓存回收与证据

全部编译/测试/进程结束后，按Ruby cleaner列出的7条精确路径回收编译缓存，逐项lsof确认无打开文件；源码、报告、baseline、runtime、EVS archive/向量、候选Products、安装与正式dist保护。回收分配空间`1910198272`字节，同期可用空间增加`1691082752`字节；2901项输入与`15936`项保留条目零漂移。没有删除整棵build/tmp/runtime、离线依赖或源代码。

[最终软件摘要](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-581-final-software-result.json)、[完整daemon2693项](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-581-final-v2-daemon-full-result.json)、[跨语言链](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-581-handoff-durability.json)、[缓存回收](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-581-cache-cleanup.json)、[下一物理提交边界审议](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-581-physical-boundary-review.md)。这些软件门通过不代表P1–P6或实机验收完成。
