# Iteration586：耐久USB观察与独立启动metadata owner

整体：接口接管 **100%（294/294）**；P1–P4同步验收、P5完善中、P6未完成。整体百分比尚无冻结分母。

耐久USB观察已入正常Intent，隔离启动接入独立有限metadata owner、held-FD完整证据及仅开既有双锁；实际释放后只留只读witness，未授予物理恢复权利。daemon 2848项通过、2个进程helper另验，定向87项、真实死亡/重启8阶段16进程通过。接口294/294由Rust接管。Release后端及包内验证通过，复用未变581 GUI/Swift；精确清理2条缓存路径、1698058240字节。未安装、无DMG、无实机。metadata到原生恢复的有限策略/实际清理/receipt/解封、helperboot及route绑定、legacy共享生命周期和完整实机/长稳/升级/性能仍待验收。

正常restore的Intent可保存实际native Transport取得的耐久USB观察：受支持VID/PID、严格有界serial/语言与bus/port链，封印capture来源并保持同VID/PID策略。synthetic ownership fixture不产生native观察。旧Intent缺失观察仍以None只读识别；有观察不等于启动时已重新证明实体设备、helperboot或AT/ADB route绑定，也不是密码学身份。

只有真正startup-quarantined的Gate可为metadata检查捕获同一实际Authority/RootProof。nonClone RecoveryOwner按operation→ADB固定顺序取得真实独占双锁，弱RecoveryRight仅MetadataInspection scope、有限deadline，真实捕获和metadata核对各边界均使用。普通OperationRight没有转换，原Gate隔离及普通restore/accepted cleanup语义保留。owner实际使right失效、反序释放两锁并核只读witness后才返回Inspection；main在生产startup实际调用、保留证据，退出前再次检查。clean启动NotNeeded无双锁副作用。

自动检查先要求原operation与ADB锁均存在且安全，然后使用实际held-root existing-only opener；不存在或检查后消失均不允许O_CREAT/重建。缺锁只是明确metadata不可检查，读卡器继续启动、module隔离保持。固定七slot（legacy pending、pristine、两锁、backup、runtime、active journal）及原absence、bounded后代、FD/stamp/actualbytes/hash均捕获；symlink/FIFO/hardlink/权限/超界/漂移失败不修证据。root私有0700，最多256nodes/4depth/单文件1.1MB/总8MiB。单次capture/verify区间full root stamp前后核对，跨观察允许无关reader文件变化；不宣称原子快照。

独立opaque journal capture沿原RootProof持有ACTIVE及全部原record FD/actualbytes/full hashchain，返回原typed Intent/steps/receipt；summary/digest只用于诊断，不作planner物理授权。普通启动仍隔离任何ACTIVE形状，归档不证明其他legacy历史clean。本包没有open/probe/STOP/replay、自动归档/删除/解封，也没有物理startup RecoveryRight或恢复完成。585的正常restore journal、same-process guard和Unknown fence继续验证；串口同IMEI重开不能代替native successor。

最终冻结2927项输入，较585变化23项/新增4项。daemon **2848通过/0失败/2忽略**，两项是独立进程helper，未当作单测通过；定向87项、FS8项、API常规9项及专项DTO1项、独立进程集成10项、包内17场景23进程、handoff3场景8进程、日志退出3模式与正常EVS10模式向量均通过。实际passed用例增删及净增42项见[新增证据](../../../../build/reports/iteration-586-new-test-evidence.json)，数字来自实际log/场景证据。没有声称整套workspace/全部Swift程序本轮重跑。

当前cut真实Service→Plan→NativeTransport字节/私有PTY及journal链的8次死亡/独立重启共16进程再次通过。每个phase/run/sequence/PID和编译artifact SHA均核对，普通业务拒绝、零factory/重放、原证据不变；故意owned SIGKILL与超时/强制收尾失败分开。最终矩阵没有强制收尾，见[当前死亡门](../../../../build/reports/iteration-586-final-v3-restore-journal-crash.json)。它验证隔离及耐久记录，不证明断电或native硬件恢复。

586原census-order失败观察保留：首次读到尚未完成发布的447scopes后退出1，等待manual review终结再顺序刷新449；该观察未运行测试/构建且未导致源码改变。586原final-v1编译失败保留（durable_binding两处缺Failure映射，0测试）；root按同文件既有unavailable映射修复，最终cut另行验证。当前已审阅失败报告共3份：iteration-586-final-v1-rust-compile-result.json, iteration-586-final-v2-daemon-full-result.json, iteration-586-final-v2-software-result.json。每份原JSON/loghash和原forced/timeout/process记录保留；未审阅失败阻止本收尾发布。585历史失败仍指原585报告/基线，不搬作586通过，也不改其RF夹具失败、v8超时/强制收尾等false记录。精确索引见[本轮门禁](../../../../build/reports/iteration-586-gate-summary.json)、[软件结果](../../../../build/reports/iteration-586-final-software-result.json)和[585历史](../../../../build/reports/iteration-585-gate-summary.json)。

另保留[v2完整daemon失败](../../../../build/reports/iteration-586-final-v2-daemon-full-result.json)及[v2软件失败汇总](../../../../build/reports/iteration-586-final-v2-software-result.json)：实际2839通过/9失败/2忽略，九项均为既有finalization_tests夹具缺physical_target/anchor，真实begin-journal准入在原断言之前拒绝未绑定AT transport。修正仅补显式synthetic ownership夹具，不代表native实体设备证明；生产门与旧目标/read-error/原释放/namespace/receipt断言保持，无新增测试函数。最终定向实际纳入旧finalization完整10项，数量由最终log动态核对；真实新增用例仍按当前/585 full差集计算，不把夹具修正重复计作新增。

另保留[定向inventory harness观察](../../../../build/reports/iteration-586-targeted-inventory-observation.json)：v2首轮在compiled --list之后、任何用例执行前正确停止。585成功full log的一例expected-panic名称带libtest显示后缀“ - should panic”，compiled --list不带该装饰；直接比对误报43新增/1个显示名移除。仅在旧display到compiled inventory的比较中去掉锚定后缀，真实新增仍由后续实际log验证；源码、测试用例、二进制和旧日志均未因此改变。原canary/inventory两log及旧full run loghash保持，tests_run=false；这不是测试用例失败，也未作为一次生产修复或新增通过计数。finalizer当前/旧full rawlog名称差集保持原样。

另保留[Swift固定origin harness观察](../../../../build/reports/iteration-586-swift-origin-observation.json)：v3收尾脚本cut替换误把固定581-v2源manifest改成不存在的581-v3，在copy/compile/test之前FileNotFound停止。原harness非覆盖基线和真实581-v2 manifest SHA保留，仅恢复两处固定源引用；本轮重新Swift动作仍是指纹复用，无新Swift编译或新Swift测试，不属于生产源码失败。

独立[真实Release main门](../../../../build/reports/iteration-586-startup-main-process.json)另有2场景/2个daemon进程，未混入17/23生命周期门、10项Rust集成或daemon单测数。通过原Suite/Daemon/canary及强制OS沙箱，两安全原锁存在时main唯一JSON诊断metadata_inspected/metadata_only/recovery_completed=false；只有operation无ADB时明确metadata_unavailable且ADB始终缺席。认证runtime保持隔离/historicalfalse，缓存reader及health仍可用，原固定七slot的metadata/bytes/absence保持，metadata_changed警告缺席，两owned daemon均正常SIGTERM0并reap。此门证明metadata接线及证据/退出，不外推整个进程factory_calls=0：正常reader discovery仍可能尝试，硬件/外网由继承的实际canary沙箱拒绝。

候选只重编Release Rust后端，未变581 GUI/Swift按源/实际二进制指纹复用；arm64、严格ad-hoc签名、许可、仅Rust/libusb及生产包无test hook检查通过。后端SHA256：`aa7290bf90d3e2a8490896f0990ec17821283fff4f633e0bfbe636f08dedacbd`。所有验证结束后精确清理2条编译缓存、1698058240字节；源码/依赖/基线/runtime/产物/正式dist保留。

仍需把metadata接到原生有限恢复策略与真正physical RecoveryOwner/RecoveryRight，重新证明耐久观察对应的实际设备/helperboot/route，实际清理、逐步readback、release/receipt和显式解封；本包未闭合这些链。legacy/shared guardian、旧3.42 exit1、未受控旧历史、真实USB/串口/no-AT/AT-ADB关系、当前候选注册/SMS/USSD/provider/来去电、9eSIM/LPA、多设备Profile、自然期限/24h及最终采用的72h门槛、原位升级/Keychain/性能和最终交付仍待验收。serial+拓扑为操作性观察，不是密码学身份。原完整P0–P6剩余台账仍见[验收审计](../../../../build/reports/iteration-585-remaining-acceptance-audit.md)，本专文仅更新本slice边界。本轮未触卡、发信、拨号、改变Profile、安装候选或生成DMG。
