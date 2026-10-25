# Iteration589：控制证据轮换、真实控制链组合与公共转义路径

2026-10-19。整体仍为P1–P4同步验收、P5完善、P6未完成；接口接管100%（294/294），整体验收分母尚未冻结。

控制证据新增有界滚动归档与实际record锁保护；12项归档和5项真实host控制/子进程/ADB分包组合通过。HTTP转义公共字面段兼容缺口修复，原4项失败保留，同一签名Release产物53项对照通过。完整daemon2905项、新增25项、包内17场景23进程及跨语言/崩溃专项通过。本轮ESTK实机读取1EID/10Profiles，确认SIMWorld454/03；注册及清理通过，用户选择暂不测试入站短信，收信仍未验收。接口294/294仅为接管率；完整HTTP矩阵、physical恢复、实机业务/长稳与P6仍未完成。未安装、无DMG。

归档只处理严格完成且实际record独占锁可得的记录。原owner/LocalCompletion保留共享锁；待确认、partial、unknown与活跃记录不删除。固定独立domain/schema的滚动archive最多保留16对完整字节和原始stamp，绑定实际root/store；O_EXCL staging与文件/目录/root同步、readback、原子提交先于精确原件移除。有效中断可重试，未知stage保持失败；它是正常完成历史的有界压缩，不是永久审计日志，不授予physical recovery或解除其他隔离原因。正常私有daemon单writer边界仍适用。

组合测试使用实际host supervisor及未reap子进程、真实UDS/HMAC回执和production ADB分包路径；仅USB/DSP与远端shell被明确替代。覆盖STOP→DSP/readback→USB release→RELEASE→checked close→durable completion、丢回执/不精确LF/关闭失败、取消、落盘重试。不能替代ARM/DSP/USB实机或完整部署组合验收。

HTTP原对照结果49通过/4失败。修复只在已clean raw path上对ROUTES公共literal segment做一次decode比较，wildcard tail、query/scheme/authority保持原始，编码slash不升格为分隔符；private/unknown保持原样。53项固定矩阵在同一签名后端全部通过，认证、HEAD/405/Allow、canonical和Range/导出头均按既定代表用例断言。完整适用矩阵、Go URL.Path/PathValue、invalid escape与mixed canonical等仍需审议验证；P2A-1.http-paths完整leaf仍不标PASS。

验证：全量daemon2905成功/0失败/2个显式专项helper，保留旧2880个通过名称；新增12归档+5组合+8HTTP。平台FS8、常规API9及独立DTO专项、独立native进程集成10、supervisor host13、Swift4运行、包内17场景23进程、日志管道3类、跨语言3场景8进程、崩溃8场景16进程、startup main2场景2进程及EVS/AMR-WB10模式向量通过。SwiftUI与工具复用已核对未变的581，Rust和Release本轮重新编译；armv7 supervisor嵌入及许可核对通过，未宣称实际ARM执行。

首轮组合fixture错误使5项失败，随后仅精确修正反斜杠断言/错误诊断，原失败记录保留。原HTTP四项失败与修复后矩阵并存，未改预期值。报告名称分析首次误用tests前缀亦已记录纠正，未改变测试。源码2975项/10项差异/3个新增输入，最终cut零漂移，原方案与115叶候选台账未改，未因本轮整理新增整体PASS。

实机：重新插拔前仍MUTE/ATR0，用户确认重新插拔后非MUTE/ATR23。当前签名后端读取SIMWorld454/03、1EID/10Profiles及正常断开/退出成功。首次注册预检在读卡前失败；独立诊断定位到隔离HOME使Surge CLI无法读取自身配置（正常环境端口匹配，隔离环境dump失败）。v2测试恢复正常用户环境供Surge CLI使用，Switch依旧显式指定全新SWITCH_DATA_DIR，保留全部隔离与接受条件，生产源码未变。传输v2实测surge-effective-profile与enhanced-mode-utun且地址一致，无host解析或代理设置变更。注册结果：注册及清理通过；用户选择暂不测试入站短信，收信仍未验收。仅注册/观察不能替代双向业务、全部设备/运营商、多Profile、自然期限与24h/72h长稳。不发信、拨号或Profile变更；所有私有runtime/SMS保留。

精确清理3条可重建缓存，共1793421312分配字节；同期可用空间变化1586847744字节，两者不混同。源码、离线依赖、EVS/manifest、ARM工具链/资源、失败证据、基线、历史/current Products及正式dist保留。安装应用/正式发行物未替换，无DMG。

后续仍需完整physical启动恢复authority/精确重连/恢复计划与独立receipt/Gate收口、legacy共享生命周期、完整HTTP/转发服务矩阵、当前真实双向通信/语音质量与设备矩阵、长稳、升级回退及整机性能。不得用正常路径archive清理推断历史物理状态已恢复。

证据：[门禁汇总](../../../../build/reports/iteration-589-gate-summary.json)、[完整软件](../../../../build/reports/iteration-589-final-v1-software-result.json)、[实机读取](../../../../build/reports/iteration-589-replug-reader-read-observation.json)、[注册与被动观察](../../../../build/reports/iteration-589-simworld-live-v2.json)、[HTTP固定对照](../../../../build/reports/iteration-589-http-final-result.json)、[缓存清理](../../../../build/reports/iteration-589-cache-cleanup.json)。
