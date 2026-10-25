# Iteration 697：配置级磁盘任务归属与严格账本基础

版本仍为3.50/build350。工作区为Switch_MAC，业务后端保持Rust；未安装App、未接入设备/运营商/外部投递，未生成DMG。P0–P6未完成，正式49父项/115叶/21类登记仍0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；分母未冻结，不报告整体百分比。

## 本批已实施及验证

两处真正可能脱离HTTP观察者的转发队列入口（enqueue_admitted、device_snapshot）使用配置级queue-I/O owner。准备future即拥有明确的Request资源；取消等待不等于真实blocking任务结束。停止配置会先关闭新准入，核对实际runner，再分别等待队列I/O与provider release，任何一项出错也不跳过另一项清理。结果/观察者两事实和有界诊断、取消stop后重等均有真实组件和生产入口测试。

审阅发现started panic只围住配置、没有围住共享QueueStore。现已在原queue guard释放前单调要求重新打开；不改磁盘、不清Unknown或原attempt token。库测试在真实rename后panic，由显式owner围栏验证缓存/盘不一致后拒写及重开恢复；daemon测试通过真实Command成功enqueue后panic验证接线。两者是互补证据，不能冒称同一个跨crate精确故障窗口；未声称正常输入已复现自然panic或丢盘。

新增默认关闭的device-send authority codec：严格原始JSON重复键/值数/深度/字节验证、类型化固定ID、完整legacy索引、同快照H+1 reserve及实际编码配额。它仅是reserve-only组件，没有现网迁移、claim、dispatch、GUI或GC接线，不改变现有v1行为。Canonical-only真实GuardedArchive测试不证明实际v2磁盘raw严格接线；给定bytes/hash也不证明活动路径来源；4KiB finish headroom仍只是预算。

## 实际执行结果

- 转发库87项、独立进程锁3项通过。
- daemon queue-I/O组件9项、生产入口集成5项、authority19项通过；转发组54、device store29、device send57、public receipt6通过。这些组相互包含，不能相加。
- 普通后端check及显式codec特性check通过。实际compiler-artifact确认普通构建无queue-fault-tests或codec特性；这是dev检查，未做Release/App包和符号审计。
- 完整daemon **3411通过、0失败、8忽略**；实际执行与3419登记精确匹配，保留前驱全部3386名称，新增33。库保留86旧名称、增加1。
- 首次daemon编译发现新测试适配器两处E0277；仅补显式StoreError::Json映射后重新验证，原失败源码/部分归档/runner/原始输出保存在attempt1，不重标绿色。
- 713项Rust输入集合、哈希和部分源码归档成员逐项核对，无漂移。归档只是Rust输入切片，不是完整冷构建胶囊。

## S11真实基线与后续边界

生产Swift Store字节前缀、完整FakeAPI前缀和原Adapters在隔离组件probe中编译运行两次；无控制/发送/通知/网络调用。1k/5k/10k冷ID churn中正文缓存止于2048项、1MiB，辅助floors/deleted/conversation revision继续线性增长。它是组件before证据，不是实际持久删除、完整UI、RSS或24/72h验收；既有静态库未冷重建。原Python证据编码失败日志保持，未把它混成probe失败或隐藏。

下一批明确global archive完成登记/poison/输出实际销毁、S10锁内raw事务与来源证明、S11持久authority/readticket后再GC。交叉审阅补出S01强制同FD复用和连接pending取消的确定性覆盖缺口；S08完整N待恢复→首次业务、部分Swift执行来源绑定与当前完整冷门仍待补。真实AF_UNIX backlog小实验在本机只得到立即连接或ECONNREFUSED，不能据此当已覆盖EINPROGRESS取消。

全项目最终三轮审议仍未执行；本批root与子代理的focused审阅不替代它。S10/S11/S12其余能力、S13当前来源/完整冷App门、S14实际24h/72h仍开放。实机、运营商、实体声音、真实Telegram/Bark投递单列未验收。

## 缓存清理

全部编译/测试停止，两个精确白名单路径lsof无占用、cleaner进程guard通过后，删除debug/incremental与旧handoff-test-target，回收4.338787GiB分配空间。保留源码、离线依赖、EVS资源、runtime、报告、源码切片、实际测试binary及当前Release静态库和产品。

证据目录：/Users/StatTrak/Desktop/Switch_MAC/build/reports/stability-697/。完整结果见execution-summary.json，原始执行在attempt2，失败保存在attempt1；最终source与registry及focused审阅单独绑定。
