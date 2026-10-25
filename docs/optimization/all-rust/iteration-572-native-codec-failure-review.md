# Iteration572：原生编解码初始化失败恢复

2026-10-19。**整体阶段：P5完善中，P6最终验收未完成；接口接管100%（294/294）单列。** 本文由预审更新为实施记录。本轮没有实卡操作、实网通话/短信、GUI/麦克风测试、应用安装或DMG，整体百分比仍待完整验收台账核定。

## 基线、收益和实现边界

原adapter仅检查外层calloc；FD-CNG内部calloc失败会解引用NULL，CLDFB第二次分配失败会泄漏父结构，初始化器忽略错误。先保留410项与Iteration571指纹完全一致的EVS源及旧production archive。受控独立进程在真实第6处分配（encoder FD-CNG父结构）返回NULL，旧版本重现SIGSEGV；同环境无注入的10种模式初始化通过。这份失败证明单列，不计入通过数，没有制造实际内存耗尽。

此包以避免明确的崩溃/部分初始化泄漏、保持正常编码结果为收益门槛，不以推测的性能提升改写DSP。内部init_encoder/init_decoder返回状态，五个CLDFB及两个FD-CNG创建点即时检查；子分配失败释放父层，不发布半成品handle，初始化入口使自有handle可安全部分析构。adapter统一销毁部分encoder/decoder并free外壳，向既有Rust边界返回NULL/Unavailable。外部switch_evs ABI、DSP算术、成功路径参数和调用顺序保留。

参考CLI及EvsRX调用方也检查内部初始化返回，失败时按各自既有责任清理；CLI完成语法检查，EvsRX随archive编译和静态审查，没有宣称完成参考CLI/EvsRX全部运行期业务验收。源码归属与本地修改范围同步写入EVS及主NOTICE，最终候选另核对实际打包内容。

真实CodecWorker原先在ready通道异常关闭时直接返回Unknown而丢下JoinHandle。现在所有未成功就绪的分支都先关闭sender并等待实际join，Unavailable/Unknown语义保持。调用者取消仍由原保留任务/owner完成清理，不能提前解除端口和资源围栏。

## 隔离的故障测量

生产archive不插入allocator hook。独立测试archive仅对native翻译单元的malloc/calloc/free做宏替换，不替换libc/Rust/测试进程全局分配器。实际constructor线程开启TLS，第N次分配返回NULL；固定有界token账本及对齐prefix记录来源，TLS结束后的释放仍归原token。free先查live registry，foreign/doublefree显式计错，不盲读未知指针前缀；所有clean运行同时核对per-token及global系统/跟踪错误。

注入器独立进程自检通过，覆盖嵌套/错误scope、4096槽上限、calloc乘法溢出、对齐、foreign/doublefree、线程隔离及跨线程释放。自检故意制造的计错不混入clean进程。native-codec-fault-tests为显式非默认feature；真实build.rs执行证明release拒绝该feature、debug缺测试archive硬失败、普通release只选生产archive。默认包无环境故障开关，完整应用另核对符号缺席。

实测EVS及AMR-WB 0–8各15个constructor分配点，冻结分母为 **10种模式、150个点**。150/150逐点注入通过：实际trace前缀对应被测点，返回NULL、没有signal、跟踪存活块/字节归构造前基线、分配/释放相等，随后同模式无注入构造可重试。不能从该分母外推所有运行期分配或OS异常。

每种模式50帧的编码bits及解码PCM，与保留的修复前独立production archive逐字节一致；EVS及AMR-WB 12.65同时逐字节核对既有独立冻结编码/解码向量。10种模式均通过，冻结oracle未更改。生产archive指纹另核对未被隔离测试改写。

## 真实线程及媒体回归

新增 **5项真实native媒体故障回归通过**，不是Fake prepare结果：

- EVS/WB真实内部末次分配失败，以及独立的ready通道关闭；出口gate证明调用尚在等待实际thread join，释放后返回既有错误且可重试。
- EVS→WB及WB→EVS替换失败时保留原codec，三阶段12帧RTP/PCM、端口/token/SSRC/序号/时间戳连续；重试及discard释放新worker。
- 双codec本地offer的第二worker实际分配失败时，等待第一worker真实退出；语音及DTMF均零TX，随后可正常准备。
- Service加真实Owner验证上述清理前不发布200或488，join之后才发送488；下一dialog可用真实双codec重试到200，ACK前仍零TX。
- 调用者取消后，实际join、PCM端口和分配围栏持续至线程真正退出，不把未来取消等同资源已释放。

普通production archive的 **301项定向Rust回归通过**，包含本地完整IKE/ESP/IMS/RTP/PCM组合及精确七条通话历史（六呼入、一呼出）。独立instrumented archive另外执行同一组call库70项回归通过；这70次是不同构建环境的重复验证，不当作新增70个功能。Swift生产逻辑harness29项模拟音频/API场景、既有device-call正确性检查通过，没有实际TCC/音频设备验收。

冻结协议297次回放通过，所有297个运行子进程由外部启动器套入强制无设备/外网、仅loopback内核沙箱；编译器在沙箱外运行。6466个direct样本仅做来源/完整性校验。native harness与5项真实线程测试同样使用强制沙箱，canary检查硬件、Mach、设备文件、外网禁止及子进程继承。

## 审查、门禁与限制

交叉审查核对C所有调用方、NULL-safe partial destroy、EvsRX重试、真实worker局部TLS与全局计错。正常成功路径静态投影一致；实际向量对照另列，不以静态投影替代执行。实际修改的11项EVS文件差异及前后SHA保存于报告。

API扫描器最初对外部cfg(all(test, feature))要求人工审阅。现在只证明精确的test与单个literal feature逻辑合取及反序，any/not/cfg_attr/任意嵌套仍拒绝，生产引用仍优先于测试引用；52项扫描器正反例通过。清单唯一差异为新增测试模块排除项，显式审阅后更新，保持294 Rust/0 Go、54条HEAD、127个Swift调用及431个既有review范围，既有指纹未变。

外部冻结回放启动器首轮错误假定原main返回整数0；原297项比较已通过，但启动器在最终返回值断言失败。已按原main无返回值的实际合同修正并完整重跑，保留失败日志；没有更改或放宽任何协议期望值。最终冻结2871项输入，新增纳入实际Xcode project/scheme及打包主NOTICE的指纹，正常Rust/C源码从定向编译到最终候选保持一致。

尚未证明native运行期挂死、OS直接杀进程、Rust全局OOM abort、实际线程spawn失败及所有资源故障；不能把150个受控NULL点与5项线程回归称为全部故障覆盖。完整SIP URI等价/quoted Via等边界、运营商接受EVS、实际双向音质/来去电/DTMF和模块DSP仍待对应验收。

正常LPA及9eSIM B0/B1认证下载、BLE恢复、真实双向短信和Telegram/Bark投递、多Profile/多读卡器N待、IMS/IKE/CHILD自然期限和24h/72h长稳保持完整范围。P6共享data-dir生命周期authority、旧3.42退出1、原位升级回退、受控整机性能及最终交付未完成。[49门验收目录](acceptance-ledger-candidate-2026-10-19.md)仍为候选，未计算分子或整体百分比。

## 最终候选与缓存清理

完整Release候选已构建，bundle ID为com.proletarius.switch.candidate572，版本3.43/343，未替换正式安装。实际包内arm64后端SHA-256为`4d92c265b8ad10a57b032f02904bbb1d713b3d216d2a39c8b5814b5ef0a10798`；deep/strict签名、仅Rust后端及libusb依赖、故障注入符号缺席、修改后NOTICE实际打包一致均通过。包内daemon在隔离沙箱的三种日志管道状态中正常退出，遗留socket为零；这些合成进程检查不代表GUI、实卡或整机性能验收。

Xcode重建/重新归档后的生产EVS archive与早期定向测试archive的原始字节哈希不同，未假定二者相同。最终生产archive SHA-256为`5795ec79a8c46a834551a0499522a595687e6842af7c9f45e2d95b6c1a73f75c`，已单独重新执行10种模式向量比较并全部通过；早期修复前archive与故障测试证据保留。最终2871项受控源码/构建输入零漂移，正常Rust/C代码从定向编译至候选保持一致。

构建及测试结束后核对编译进程和打开文件，精确清理9项可重建缓存，回收分配空间2,576,510,976字节（约2.58GB）；同一观察窗口的磁盘可用空间增加2,348,392,448字节（约2.35GB），两者分别记录。源文件、离线依赖、EVS资源/归档/向量、回退基线、候选和原正式dist均保留并核对指纹。最终[门禁报告](../../../../build/reports/iteration-572-gate-summary.json)与[源码审查](../../../../build/reports/iteration-572-source-review.json)已完成，目标保持active。本轮无DMG。
