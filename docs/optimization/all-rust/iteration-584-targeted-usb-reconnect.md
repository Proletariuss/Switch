# Iteration584：精确USB重连与释放后的还原回执

整体：接口接管 **100%（294/294）**；P1–P4同步验收、P5完善中、P6未完成。整体百分比尚无冻结分母。

普通模块USB重连已接入原设备锚、唯一候选与完整身份校验；还原回执等原预约真正释放后发布。daemon全量2767项及60项定向通过，接口294/294由Rust接管。构建并验证Release候选，复用未变GUI；精确清理2条缓存路径、1691328512字节。未安装、无DMG、无实机。独立启动恢复、耐久journal及完整设备/长稳/升级验收仍待完成。

普通call-mode操作在首个持久写入前，从已持有的实际AT连接捕获原Device、严格完整USB serial/语言及bus/ports。重连不再调用无参发现factory；同一实际Device精确重开，或在持有Context的完整快照中确认原对象离线，再对原位置的唯一、同类型候选核对serial后claim/probe。候选先保留在私有transport，完成typed CGSN/ATI读取、IMEI相符且流未标记retire后才发布session；另台设备即使IMEI相同，也不能替代原目标。

重连权限绑定原radio/USB owner、代次、预约、root和操作锁。新重连proof的克隆与anchor不强持原Guard/锁；实际重连blocking worker另持强pin直到清理结束，并阻止提前release。既有预约和配置CommitProof的持锁语义保留。已观察到的root/锁归属失败会令旧proof克隆失效。QCFG模块VID/PID与外侧USB描述符分别处理，仅改功能位不被误判成设备类型转换。配置未知结果不会因仅同IMEI而清除，只有对应已提交重启的已确认successor能结算该次重启unknown。

还原目标回读通过后只产生进程内待发布结果；外层owner先等原预约及借用任务真正释放，再核对当前producer/root、原备份与原锁路径，最后发布last_restore。独立只读证据FD固定原inode，但不共享被flock的open-file description，不延长排斥，也不授予新的设备操作权限。它不是持久journal。

验证：最终daemon **2767通过/0失败/0忽略**，定向60项；平台原语8项、API常规9项及专项DTO1项、独立进程集成10项、候选17场景23进程、日志退出3模式、跨语言持久化3场景8进程及EVS10模式向量均通过。daemon相对583净增27项；实际用例名增删清单见[新增证据](../../../../build/reports/iteration-584-new-test-evidence.json)。14项共用生产selector/parser/probe算法测试与7项真实Service→Owner软件测试均使用明确的物理I/O替身，不能外推成原生USB实机通过。

保留失败：首次编译6项接线错误，未运行测试；v3全量2766通过/1失败，暴露原操作锁错误码被泛化，生产映射修正且原断言保留。初次定向脚本最低计数与嵌套filter有误，实际35项全通过；最终60项和无筛选全量补齐了相应范围。所有原日志/报告保留，见[最终门禁](../../../../build/reports/iteration-584-gate-summary.json)及[完整软件结果](../../../../build/reports/iteration-584-final-software-result.json)。

取舍：Release告警由133增至136，新增3项均为未使用的导出/访问器，无已证实运行时收益。单独清理会再触发冻结、编译和完整回归，本轮三项实际耗时合计约358秒；合并到下次必要源码改动清理，未自动cargo fix或为消除告警改变行为。见[取舍记录](../../../../build/reports/iteration-584-warning-tradeoff.json)。

支持边界：严格serial/完整拓扑不可得时，首个持久写入前拒绝；本轮没有原生描述符/重枚举实机验收。只支持原实际设备类型的受限重枚举，每个有限预约最多一次已确认受控重启；重启但USB不断线、跨VID/PID转换、serial/no-AT及多设备矩阵仍待适配/验收。serial和拓扑是操作性连续性证据，不是加密硬件身份，元数据检查与系统调用也不是原子快照或硬时限保证。

独立StartupRecovery owner/coordinator、耐久intent/逐写journal/最终receipt、legacy/shared guardian及P0–P6剩余必需验收尚未完成。上次580 ESTK实机停在PRESENT+MUTE/ATR0，本轮没有再次探测、发信、拨号或改变profile，也没有安装候选或生成DMG。正式dist与已安装应用保持。

下一包接入真实还原副作用之前的耐久Intent、每次实际写入的Prepare/known/unknown记录，以及release后的耐久receipt；崩溃重启仅识别未完成记录，禁止自动重放未知写入。当前新增锚和有限普通操作proof不能转换成启动恢复权限。
