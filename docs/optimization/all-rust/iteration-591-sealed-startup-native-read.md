# Iteration591：sealed startup-v1只读原生选择与main验收边界

2026-10-19。独立sealed startup-v1只读owner接入真实main；完整daemon实测2964项通过，保留590全部2927个通过名称，新增37项（按完整日志差集）。规定24软件报告及真实bound-unknown journal的main沙箱门禁通过；只读身份/配置policy不等于恢复写入、receipt或Gate解封，USB实机读回仍未验收。HTTP沿用590固定94项：92项执行断言通过、2项year1跳过未验证，整体HTTP及leaf仍未通过。591无实机/运营商测试，589注册仅属历史；用户本轮不测入站短信。接口294/294、100%仅为接口归属；产品P1–P4同步验收、P5完善、P6未完成，整体不计算百分比。未安装、无DMG。

独立非Clone执行owner绑定当前Gate/root、原始operation与ADB内核锁、真实typed journal及原字节证据。startup-v1只允许唯一完整bound journal输入；旧reservation和DurableUsbBinding始终是数据。真实blocking worker经一次fresh Context与完整最多256设备快照，按完整bus/port全部占位唯一化，再检查外部model、LANGID、严格UTF-16 serial及精确AT interface/alternate/bulk tuple；不重建旧normal anchor、不伪造PhysicalTarget、不fallback到其他设备/接口。

拓扑、serial与IMEI只提供可观测连续性，不是密码学硬件证明。缺失或旧unbound journal、拓扑迁移、external model变化及仅ADB情形继续保守拒绝；不通过序列号扫描或普通发现补造旧权利。

只读指AT身份/配置语义；host层允许claim和唯一存储alternate的SET_INTERFACE（含0），再用有限GET_INTERFACE单字节严格读回。禁止set_configuration、其他alternate/interface fallback、AT配置写与重启。固定AT与三组readonly Plan严格核验IMEI及实际USB/IMS/VoLTE值，异常、撤销和panic保持显式checked close，NoDevice仅为host资源退役，不等于当前连接、配置恢复或历史清理成功。scope、证据与deadline在边界检查；无timeout参数的native API不因此获得硬wall-clock截止保证。

验证以final-v1为准：源码2979项，相对实际590/2975输入共11项union差异；完整daemon实际2964成功/0失败/2个独立专项helper，旧2927个成功名称保留，新增37项，实际名称与分组见完整日志连续性报告。生产审议绑定7个实际文件及8个审议范围，不沿用旧轮固定计数。24项规定软件报告外，新增真实bound未知1步journal的Release main验收：父运行冻结Rust exact测试并证明唯一新增root，驱动不复制或编造journal/锁；在强制硬件/网络拒绝沙箱下要求native pending、原unknown history/原inode字节不变、cached reader/health可用与正常SIGTERM退出及锁退役。该沙箱验收不声称实际native Context被instrumentation观察，更不代表USB serial/claim/AT实机成功。

当前能力是只读startup policy与有限观察，不包含恢复写入/重启、原voice/helper清理、durable recovery receipt或Gate unseal。普通module quarantine与unknown journal保持。完整physical恢复、legacy共享生命周期及产品验收仍未完成。

HTTP仍绑定未改的590 fixed94矩阵、原主来源及预期SHA；92项执行断言通过、2项year1由文件系统拒绝或钳制而跳过未验证。representative、normal、leaf与整体HTTP均为false。实际591 Release后端、候选签名、生命周期、跨语言落盘、崩溃、startup及10模式向量绑定各自报告；SwiftUI与Swift工具复用未变且已核验的581原物，不宣称本轮重编GUI。

591没有实机或运营商验收，用户本轮不测入站短信；589历史注册/读取保留原版本和二进制身份，不能转标为591业务验收。当前收发、语音、设备/运营商矩阵、自然期限及24h/72h长稳仍未验收。

精确清理3项可重建缓存，分配字节1798946816；同期可用空间变化1591279616字节分别报告。保留保护源码、离线依赖、normative plan/台账、EVS/ARM资源、正式dist、已安装应用、571–591及所有old Products、失败原始证据、591原fixture runtime和历史硬件报告；不把runtime/fixture/baseline/产品目录当缓存。should-panic名称后缀解析及规范计划独立精确pin例外保留。旧590首次编译失败和HTTP反例原报告不覆盖、不洗白。

591首次startup门禁失败来自generated driver把“原两锁存在、无journal”的Ok(None)误判为必须有Err日志；修正仅涉及验收驱动，源码/Release未改、无需重编。root在同一source与Release上重跑后两场景正常退出通过；初次失败JSON、日志与旧driver的原字节独立归档并保留引用映射，不改写原失败结果。HTTP原command仍exit1/passed=false；92项实执通过不消除两项OS未验证，分类报告不能升级成整体HTTP通过。

接口294/294的100%只描述接口归属。产品P1–P4同步验收、P5完善、P6未完成；候选叶/父项不构成冻结整体N，不新增整体PASS或整体百分比。未安装、无DMG。

证据：[汇总](../../../../build/reports/iteration-591-gate-summary.json)、[完整软件](../../../../build/reports/iteration-591-final-v1-software-result.json)、[名称连续性](../../../../build/reports/iteration-591-test-inventory-continuity.json)、[生产审议](../../../../build/reports/iteration-591-final-production-review.json)、[bound main](../../../../build/reports/iteration-591-final-v1-bound-journal-main.json)、[fixture来源](../../../../build/reports/iteration-591-final-v1-bound-journal-fixture-provenance.json)、[HTTP](../../../../build/reports/iteration-591-http-final-result.json)、[优化准则](../../../../build/reports/iteration-591-optimization-criteria.json)、[清理](../../../../build/reports/iteration-591-cache-cleanup.json)、[590原失败](../../../../build/reports/iteration-590-final-v1-rust-compile-result.json)、[589历史注册](../../../../build/reports/iteration-589-simworld-live-v2.json)。
