# Iteration 090：串口模块初始化与短信就绪门禁迁入 Rust

日期：2026-10-05。延续全 Rust 后端主目标；所有修改、缓存和模拟测试材料位于 Switch。未启动生产 App、读取生产数据、连接实卡、发送短信、拨打电话或生成 DMG。语音目标保持EVS优先、AMR次选、无PCMA；当前实际主机codec仍是AMR-NB，EVS/AMR-WB未接入。

## 本轮接管

Rust串口actor现在执行完整启动顺序：等待150ms，AT探测；探测成功后执行ATE0、CMGF=0、CNMI、CLIP、QPCMV，每条配置完成后等待100ms，每条命令2秒超时。等待由actor状态表示，不在串口线程sleep，Close和URC仍可处理。所有命令进入既有普通优先级队列，没有创建另一串口持有者。Go生产路径改为一次认证私有`POST /api/internal/modem/initialize`，不再逐条安排AT。

- 初始化绑定串口session。同会话正在进行时返回明确busy，完成结果缓存；调用者丢失ACK也不会导致Go回退或重发初始化。关闭取消deferred/queued/pending任务，新端口会话清空旧结果，旧完成不能使新会话ready。
- 返回有限的completed、probe_succeeded、sms_pdu_ready及固定步骤/结果列表，不输出原始AT响应或身份数据。Go严格校验字段存在、步骤顺序和布尔值一致性；未知或不一致响应失败关闭，不降级到旧逐条执行。
- 原ready表示初始化尝试已结束，并非每项配置都成功。探测失败保留该ready语义并停止设备信息采集；探测成功后可选配置失败仍继续其余步骤及原有collectDeviceInfo。SMS另要求整个初始化完成、AT及CMGF都得到OK；提示符不能充当PDU模式确认。
- 初始化前CMTI仍按原观察代次标记Rust归属并进入有界队列，避免绕回Go读取。worker等待PDU就绪；直接typed存储read、confirm、delete也受相同门禁。进行中的scan只表示入队，不表示已读取；初始化已失败时拒绝新scan，保留卡内来源和已排队通知。不会自动重放初始化，重新建立连接才能开始新的初始化尝试。
- 新增独立`initialization_owned`打开能力请求/确认，和既有`sms_receive_owned`分开。旧辅助进程的接收能力声明不会暗中开启其无法完成的初始化门禁；新Go遇到旧Rust缺少初始化能力时明确拒绝。旧独立串口/测试fallback暂留，不计作已去Go。
- 连续普通AT超时触发watchdog时，由Rust立即关闭初始化会话、撤销队列、保持短信不可读。初始化RPC和正在等待/后来的Read均保留`serial_timeout_watchdog`类型，Go映射回原`at_timeout_threshold`恢复原因；两条路径并发时只触发一次恢复。
- Go初始化及随后采集的协程纳入实际退出等待。Go仍采集设备信息、维护显示缓存、执行默认飞行模式、模块VoWiFi占用和外层重连策略；本轮没有声称这些职责已经迁完。

## 验证

- Rust daemon全量439项通过；随后新增pending-read watchdog和独立能力协商回归，最终modem actor定向68项全部通过（373项过滤，当前总数441；没有把它表述为一次全量441项运行）。新增14项覆盖六步时序、配置失败、重复请求、队列满、丢失ACK、跨会话关闭、PDU就绪、watchdog及旧协议兼容。
- Go modem全包race：141个顶层测试、237项含子测试通过；定向接线race：19个顶层、69项含子测试通过；最终独立能力字段修正后的adapter复跑：10个顶层、30项含子测试通过。以上均无失败或跳过。
- 测试日志位于Switch根目录的`build/optimization/2026-10-05-modem-init-{daemon-tests,pending-read-test,final-actor-tests}.log`和`2026-10-05-modem-init-{modem-all-race,directed-race,adapter-final-race}.jsonl`。

- API契约人工审阅后，清单更新及一致性检查通过：199项method/path，Rust99、Go100，Swift调用点127，指纹审阅范围103。工具16项自测通过；`--require-rust-only`仍按预期拒绝默认Go fallback及124条Go归属/依赖路由，未把此次接入当作Go退出。
- 契约日志：`build/optimization/2026-10-05-modem-init-contract-{review,write,check,selftest,rust-only}.log`。Rust workspace格式、独立include测试文件格式及`git diff --check`通过。
- 另两位代理交叉审阅实现与退出语义，并补充watchdog pending-read回归和类型化恢复原因。Go manager含raw字符串，现有通用scope词法器不能完整提取；该文件的初始化/退出/watchdog变化由人工审阅和race覆盖，不伪造自动指纹证明。

伪串口和假HTTP覆盖实现接线，不替代实体设备、运营商或长期运行验收。

## 取舍和后续

本轮优先移动真实生产初始化职责，未迁当前macOS没有调用的USSD薄适配，也未只将USB轮询的8秒timer搬入Rust。下一步仍需迁完整设备状态/占用协调；Go的busy布尔值不能作为Rust reservation的替代。

初始化完成后没有提前自动扫描：继续保留archive初始化、RF探测抑制、Start/WaitReady、默认飞行模式、eSIM初始化、scan请求的既有顺序。Rust接到scan后决定是否入队及何时读取，不把初始化完成误认为RF策略已经应用。Go WaitReady超时也不能绕过Rust的PDU门禁。

设备信息采集的分阶段缓存与RF抑制切换、USB物理transport、读卡器/IMS/N待会话、通话事实owner、实际硬件恢复和最终Go退出仍需推进。新初始化模式只确认本次六步执行，不声称已经识别并阻止之后用户通过AT控制台主动更改短信模式的所有情况。9eSIM 6982继续排除。

探测和PDU模式确认必须收到OK；异常的提示符不会被当作成功。旧Go通用AT执行器可能把提示符返回为无错误，因此这项属于明确收紧异常响应判定，不声称异常路径逐字等价；正常六步顺序及后续设备采集顺序保持。

后续通话状态迁移应同时覆盖模块CS与模块IMS共同使用的module身份，及独立reader身份，避免Go和Rust分别修改同一通话。结束记录固定ID和内容后交Rust持久化；保留重复RING、迟到号码、无号码活动状态、挂断去重和归档失败重试语义。CLCC查询目前仍受Go的SIM/USB转换锁保护，迁移状态判定不能绕过这些锁直接争用设备。这是下一阶段审查结论，本轮未实施，也未计入进度。

P2B按已接入生产初始化与离线验证证据由35%增至35.5%，增加0.040计划人周；P2A保持59%。原计划基线合计10.795/47.5≈**22.7%**，误差约±3个百分点。EVS新增平台/codec工作仍在原迁移估算之外，不提前计算为已完成；本百分比不是可发布或实机验收比例。
