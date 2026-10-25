# Iteration 695：接入期限与容量修正

状态：本批当前冻结源码的软件回归已通过；完整项目目标仍未闭合。最终预算/HTTP
21项通过，正式后端check通过，完整Rust daemon 3374通过/0失败/8忽略。
版本保持3.50/build350。
本批不生成DMG、不安装App、不测试真实卡片/运营商/Telegram/Bark投递。

## 实施范围

对694独立库存的81处编译收包点逐项复核：79处现役网络入口中73处迁移、6处原共享
实现保持；2处有限本地Full DTO适配器保留。所有现役入口使用显式绝对期限和定期
让出执行器的共享collector。既有2/3/5/15秒和400/408/413等错误合同保持；原无期限
入口新增默认5秒、typed408。期限只包接收，不包已受理的owner、磁盘提交或真实清理。

少数Invalid底层流错误的自由文本改为固定`invalid request body`，保留status/code/
JSON结构及原操作前缀，TooLarge原文保持。此处是记录在案的诊断文本变化，不宣称
所有error字符串逐字不变。具体逐收包点记录在三份`*-ingress-change.json`。

`request_body_budget`补齐35个此前缺失method/path，USB三项SIM RPC直接引用实际
1MiB collector常量，修复此前1MiB收包只记64KiB的16倍差异。挂断、断开、多待停止、
reader owner、call-mode清理及EAP release等精确路径使用控制池；普通池与控制池各
8MiB，以64KiB取整。该预算计接收正文的逻辑字节，不是进程RSS硬上限；解析对象、
allocator capacity、socket与其它缓存另计。控制池被自身流量耗尽仍可能暂拒新请求，
本批通过收包期限归还容量，不承诺控制池永不耗尽。

回归fixture记录156个独立审查method/path，家族路径选实际代表。它检查冻结库存，
不自动发现未来新路由。主入口的认证、方法、代际和物理权限仍独立生效。

## 执行证据

所有原始输出与JSON在`Switch_MAC/build/reports/stability-695/`。

| 项目 | 当前结论 |
| --- | --- |
| 第一次统一编译 | 101；10个导入/测试参数/类型错误，已修正，原失败保留 |
| 修正后编译 | 通过；随后删除15个编译器证明无用的BodyExt导入 |
| 首轮定向 | 27通过/0失败；绑定`rust-source-focused.json` |
| 64路组合首轮 | 20通过/1失败；测试夹具丢失sole Arc对照对象，原失败保留 |
| 修正后最终预算/HTTP | 21通过/0失败/0忽略，10.58秒；保留owner.clone以核查实际状态 |
| 正式后端check | 通过，22.083秒；仍有既有/test-only imports及dead-code告警 |
| 完整daemon | 3374通过/0失败/8忽略，libtest1840.19秒；8个专用子进程helper不计通过 |
| 格式/差异 | 68个变更Rust源rustfmt check通过；diff check通过 |
| 测试名称 | 3382个，较694新增13、删除0；名称登记不代替实际执行结果 |

64路测试是production Hyper parser + 私有实际Budget + CleanupGate + ordinary
handler的组合，在main的64连接上限规模执行；无设备daemon acceptloop/认证全链
未在该fixture启动。64条未完成正文按既有5秒/400返回，控制容量全部归还，关闭门禁
成功，原claim仍在；后续完整status请求可占用返还槽。它不把收包退出当物理退休。

三位High子代理交叉审查他域实现，root整合与反查。三份S07 scoped review不是最终
三轮全项目发布审议。root后续机械导入清理及64路测试修正另留证据，最终编译/全测
绑定当前707项Rust/manifest/fixture输入。既有rust-toolchain.toml是新纳入追踪，
并非本批新建；694的705项清单未含它。`reviewed-rust-inputs.tar.gz`每个成员已逐项
复核，仅是部分Rust来源切片，不是完整项目或冷构建胶囊。接口来源指纹/库存仍待S13
人工复核，不批量改hash伪造通过。

## 下一批与新发现

当前生产源码未包含下列方案的实现；均不能计PASS：

- S10：先实现默认关闭的共同typed authority，再做device-send完整身份签发/GUI
  journal/public+internal全链迁移。不能删v1任意ID/unknown来回收容量。
- S11：先补authoritative archive scope、observation/minRevision和request-generation
  屏障，再回收辅助水位；全摘要和用户已加载历史仍需独立分页/显示契约。
- S12：public receipt纯磁盘事务、队列完整保留命令、纯YAML/IMEI事务分别隔离。
  Audio需先保持owned admission，否则取消旧claim可晚落盘反超latest序号。

本地外部harness使用真实ForwardingWorker/QueueStore和显式localhost proxy，复现
取消型Unknown的同Store人工retry被旧volatile chunk标记阻塞；自动flush没有新增
连接。所有mock socket/线程已join，无上游连接/系统代理修改，不证明真实供应商投递。
原始证据为`unknown-retry-harness/result.json`，typed provider Unknown仍待独立测试。

下一批执行决定见`forwarding-attempt-ownership-design.md`：durable begin之后签发
独立attempt token，旧ACK/Drop不能修改新owner，人工retry仅在持久成功后清已退休
chunk标记；同时修复AfterCommit被公共回执误记completed的跨层分类。只清map不足以
安全解锁。方案预审与源绑定分别记录，未作为已完成代码或最终审议。

## 整体进度和边界

正式49父项/115叶/21类，0 PASS / 5 PARTIAL / 110 NOT_REVIEWED保持。P0–P6
仍未完成，审定分母未冻结，不计算整体百分比。S10退役、S11剩余元数据、S12实际
执行器隔离/测量、S13同源冷验收及S14真实24/72h连续运行和最终三轮复核仍开放。
实机/实网、真实音频、外部通知投递按用户范围单列未验收。

缓存清理：本批所有build/test结束后，Ruby白名单及进程检查通过，lsof确认精确
incremental目录无打开文件。实际仅删除`build/rust-target/debug/incremental`，回收
8,042,524,672分配字节（7.490GiB）；保留离线依赖、测试二进制、当前产物、源切片、
运行恢复数据和原始成功/失败证据。未删除整个Rust target或任意用户源码。

707项冻结输入在测试前后包含增删检查，全部一致；最终源切片逐成员重新核对通过。
`execution-summary.json`记录首编失败、64路夹具失败和各次修正后结果，未覆盖失败证据。
后续转发缺陷会在新批次实施，本批通过不能代替其修复验收。
