# Iteration573：进程互斥与运行中媒体退出

2026-10-19。**整体进度：P5完善中，P6最终验收未完成；接口接管100%（294/294）。** 本轮实施进程和媒体资源生命周期修复，最终执行结果以本轮门禁报告为准；未测内容不计通过，不生成DMG或替换正式安装。

## 已复现的基线

使用Iteration572候选包内后端（SHA-256 `4d92c265b8ad10a57b032f02904bbb1d713b3d216d2a39c8b5814b5ef0a10798`），在新建合成数据目录、强制禁止硬件和外网的内核沙箱中启动两个真实进程。两者同时存活且使用相同data-root；第二进程替换public/callback/internal三个socket inode，第二进程退出后三个路径消失，第一进程仍存活。两进程最终各自正常退出，无强杀。这是明确的不安全对照，单列保存于`build/reports/iteration-573-before-duplicate-daemon.json`，不能算作通过用例。

原因是启动先打开并恢复业务store，缺少贯穿进程的data-root租约；socket准备直接删除既有socket，退出guard又只按路径删除。Swift对象内的重复启动检查不能覆盖CLI、多个App或并发检查窗口。

另一个代码缺口是运行中codec线程退出后，断开的通道被当作短时队列满，RX路径吞掉错误，没有空闲唤醒的worker健康信号。实际线程已经join且媒体端口关闭后的运行错误，也可能使旧Entry永久保留。实现与验证须区分运行失败和资源退出未确认，不能靠提前解除围栏解决。

## 实现范围

新daemon参与共同的data-root准入协议：在任何配置初始化、store打开/恢复及设备owner创建之前取得目录inode及固定锁文件的双重排他租约。生产租约放在process-static中，不能因run future退出或取消而提早释放；OS进程退出才释放。目录别名归一，锁条目校验普通空文件、单链接和身份，锁文件永不删除。不同data-root的合成进程可以独立运行。

public/callback/internal三个socket各有贯穿准入与清理的固定companion Claim，串行化不同data-root对相同遗留路径的准备，companion永不删除。socket准备改用非阻塞connect；Darwin满队列也可能返回ConnectionRefused，所以还须关闭诊断probe，再以当前UID及固定`/usr/sbin/lsof`确认无owner（有界输出/超时，只有exit1且双流为空才接受），最后复核同一socket inode/dev。活跃、饱和、权限及未知结果均保留。退出guard只清理本次捕获的socket身份。普通metadata检查后unlink并非原子，不能声称抵抗非参与进程或同UID持续路径替换，也依赖平台owner可见性。

媒体端为真实worker增加运行退出监测，无RTP/PCM时也可撤销就绪；通道断开是致命故障，Full仍可恢复。实际join已经完成的panic保留诊断但不冒充清理仍未确认；join协调或UDP关闭未确认则保持资源围栏。Service对已确认的致命媒体错误，在健康tick和ACK/协商提交/可靠PRACK等请求路径等待信令回执前先撤销ready/early/port/token，保留实际资源占用到退出回执。可恢复的prepare失败+discard成功保留旧流，retired dialog不撤销新通话。未声称普通用户hangup/CANCEL的所有路径也已具有该首次await边界。

## 已执行的限定工作包

| 验证 | 结果与边界 |
| --- | --- |
| 普通Rust定向回归 | 338通过，0失败/忽略；含本地完整IKE/ESP/SIP/EVS/WB/RTP/PCM及精确七条通话历史 |
| 真实native故障 | 16通过；另instrumented call70项是重复回归，不是新增功能数 |
| 请求竞态 | ACK先于tick；UPDATE200已交writer、成功回执阻塞期间worker死亡；PRACK实际WB构造失败、阻塞200时先撤桥，500和实际join仍须确认 |
| 真实包内进程 | 17场景、23进程通过；正常/满/闭日志管道3种退出通过，无超时或强杀；仅新daemon合成root，不是物理/旧版恢复证明 |
| Swift | 音频状态29个mock场景、设备通话逻辑、readiness现存socket保护通过；未启动GUI/麦克风 |
| 冻结协议与接口 | 297次协议回放、52个扫描器测试及Go退出通过；294/294Rust，54派生HEAD、127Swift调用、431复审scope |
| 完整Release候选 | 严格签名、arm64、只含Rust后端及libusb，无故障注入符号；实际daemon SHA `dca2a6a2905640db8ad95a0051a63daec3af30bf911c7ec45510d8016c476c6d` |
| Native向量 | 最终archive重新比较10种模式通过；410份native输入未变，572的150分配点证据保留，未冒充本轮重跑 |
| 输入 | 2890项冻结输入零漂移；包括既有许可/工具链/CI及API复审元数据的扩大监测，新增监测数不是新增功能数 |

上述运行均强制拒绝硬件与外网，编译在运行沙箱外。候选位于`build/iteration-573-derived`，正式dist/DMG未替换，也没有新DMG。完整报告：`build/reports/iteration-573-gate-summary.json`。

保留失败及修正记录：最初13项native中2项peer发送WB但实际协商EVS，修正单codec offer并使用真实协商codec/PT，所有3秒/4帧/语音/零TX断言保留；Swift压力计数改用自身Mach线程清点，避免运行setuid ps。stale测试的刚关闭listener在80次独立观察中有1次connect=0，生产此时拒绝删除正确；改为从未listen的遗留bind节点后8项连续20轮通过，未添加sleep或放宽生产规则。所有先前失败单独存档，不计通过。

## P1–P4同步验收的新证据

用户已要求P1–P6全部完成验收。只读核对形成P1/P2的21个、P3/P4的11个候选门条件/证据/版本清单，均未据此宣布阶段完成。当前normal daemon全量2518项软件门禁实跑为**2377通过、141失败、0忽略**，完整软件门禁保持失败：122项涉及伪串口被沙箱拒绝，13项测试夹具试图写源码目录，5项平台trust边界尚未闭合，1项refresh的实际AKA入场/OPTIONS计数时序待验证。后续只能修正运行条件和夹具同步，再重跑；不能跳过或降低断言。

针对5项trust测试，特定trustd及securityd Mach服务允许的对照仍为2通过/5失败；此时已跨过正例trust evaluation，却在依赖的nullable旧public-key wrapper处panic。只能定位失败阶段，不能断言现代API已解决或全部属于沙箱。待审修正必须保留系统信任与真实签名验证。

IORegistry已检测到ESTKme-RED，用户确认使用SIM World（3HK）。完整候选验证后，准备独立root的严格注册/清理验收；已安装后台PID4114在SIGTERM后195秒仍存活，未强杀，强退选择待用户回复，尚未开始本轮实卡注册，不能移用559的注册结果。软件修复继续推进；编译缓存正用于这些失败的定位与重跑，暂保留，完成后按精确路径清理。

## 全项目边界

本轮租约仅约束参与本协议的daemon，不证明未包装旧版不会运行，也不证明父进程被SIGKILL后所有ADB probe子进程都已结束。内部单次ADB probe仍有独立接口锁和父supervisor的正常kill/wait责任，不能因新daemon全局锁可取得就宣称设备已恢复。跨不同data-root的同物理设备归属亦须相应验证。

持久pristine marker不能独自证明跨旧版的clean lineage；正常模块备份后的恢复准入、共享动态authority、3.42原退出1及完整原位升级回退仍未完成。没有把合法模块功能永久隔离来缩小验收范围。

实网EVS、双向语音/短信、正常LPA及9eSIM认证下载、BLE恢复、真实Telegram/Bark投递、多设备N待、自然期限与长稳、整机性能和最终交付均保持完整范围。49门目录仍是候选，整体百分比尚无冻结分母。
