# Iteration590：原生关闭回执、重连围栏与HTTP边界

2026-10-19。原生AT资源关闭与重连候选失败保留补强；完整daemon实测2927项通过，保留589全部2905个通过名称，新增22项（关闭/重连16项、HTTP包6项，按完整日志差集）。规定软件专项、候选包及退出链通过；HTTP固定94项中92项执行断言通过、2项year1时间被文件系统钳制而跳过未验证，完整HTTP门禁及leaf仍未通过。本轮未运行实机/运营商测试，589注册仅为历史；用户暂缓入站短信。接口294/294仅为归属，整体P1–P4同步验收、P5完善、P6未完成。未安装、无DMG。

原生关闭现在区分已释放并Drop句柄、设备已断开并Drop句柄、关闭未确认；OwnerLease覆盖完整资源退役。NoDevice只证明断开目标的host资源结束，不能证明SET_INTERFACE、DSP/语音/USB配置恢复，也不授予物理恢复权。未发布候选和opener失败绑定原Anchor；原连接的关闭债务阻止同Anchor再次打开，独立新操作不擦除旧债务。软件夹具不能替代ARM、DSP、USB实机验收。

HTTP按保留的Go与应用源码修复规范化时二次转义、无效path escape、operation参数一次解码，以及导出条件、Range、错误头和1969/UnixEpoch日期边界。native短信导出token与message_id原契约保持。固定94项中92项实际执行并通过；year1时间在本机被钳制，2项明确跳过未验证。representative、normal及leaf均为false，不写成整体HTTP通过；极端日期和其余适用矩阵仍待核查。

验证以final-v2为准：源码2975项、相对589最终cut共38项差异；完整daemon实际2927成功/0失败/2个专项helper，旧2905个成功名称保留，新增22项，其中关闭/重连16项、HTTP包6项，名称及实际分组见连续性报告。API/FS、Swift复用工具、Release候选签名与包内生命周期、跨语言落盘、崩溃、startup main及10模式向量按各自报告通过；SwiftUI与工具复用已核验未变的581，不宣称本轮重新编译GUI。最终生产审议绑定13个文件/15个范围和实际源码、测试二进制；早期HTTP日期源码哈希仅由root类型适配替代，以v2为准。优化准则保留其基线、阈值和取舍，不把软件正确性测试当成整体性能验收。

失败和计数纠正均保留：首次final-v1编译因time解析缓存声明类型不符而失败，root作最小类型修正并重冻v2；review范围首次误计16，断言在写源码前停止，实际15个范围及原身份列表核对后重跑。矩阵预期误用589下载夹具名，19项文件名预期按固定590夹具纠正；原矩阵、驱动、首次与校准后旧589反例并存，不把错误oracle的失败隐藏为生产回归，也不删除真实基线缺陷证据。

本轮没有实机或运营商测试。589已记录的注册/读取仅属于历史源码与二进制，不能转标为590验收。用户本轮暂缓入站短信，未提交发送、拨号或Profile变更；当前双向业务、设备/运营商矩阵和长稳保持未验收。

精确清理3项可重建缓存，分配字节1796726784；同期可用空间变化1588989952字节，两者分别报告。源码、离线依赖、原方案/候选台账、EVS与ARM资源、失败基线、571–590 Products、历史实机证据、安装应用及正式发行物保留。115候选叶/49父项尚非冻结N，不计算整体百分比；未新增整体PASS、未安装、无DMG。

后续仍包括完整physical恢复authority/精确重连/receipt与Gate、legacy共享生命周期、完整HTTP与外部转发服务、当前真实短信/语音及设备矩阵、自然期限和24h/72h、升级回退与整机性能。

证据：[汇总](../../../../build/reports/iteration-590-gate-summary.json)、[完整软件](../../../../build/reports/iteration-590-final-v2-software-result.json)、[名称连续性](../../../../build/reports/iteration-590-test-inventory-continuity.json)、[最终生产审议](../../../../build/reports/iteration-590-final-production-review.json)、[优化准则](../../../../build/reports/iteration-590-optimization-criteria.json)、[HTTP实际结果](../../../../build/reports/iteration-590-http-final-result.json)、[旧589校准反例](../../../../build/reports/iteration-590-http-589-counterexamples-v2.json)、[首次编译失败](../../../../build/reports/iteration-590-final-v1-rust-compile-result.json)、[范围计数纠正](../../../../build/reports/iteration-590-review-count-correction.json)、[缓存清理](../../../../build/reports/iteration-590-cache-cleanup.json)、[589历史注册](../../../../build/reports/iteration-589-simworld-live-v2.json)。
