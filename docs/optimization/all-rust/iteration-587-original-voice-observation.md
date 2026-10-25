# Iteration587：原语音 helper 耐久观察与验收台账

2026-10-19。整体仍为 **P1–P4同步验收、P5完善中、P6未完成**。接口接管 **100%（294/294，含54条HEAD）**；整体验收分母尚未冻结，不以接口、测试或候选叶子数量计算完成率。

## 本轮实际改动

正常还原在写入Intent前，排空并持有真实ADB admission，从原进程记录捕获语音义务；guard跨Intent耐久提交、restore fence和原radio lease attach保留，写入前后再次核对归属，随后释放guard再进入原清理。正常Start/Stop及原Native open正文未改。

新DTO区分 current_process_empty、original_owner_no_owned_cleanup、unknown 和 owned。新进程空registry不证明历史已清理；旧schema1缺字段保持None/Unknown，原文件不重写。Intent拒绝不同producer的观察。

Owned仅由原生路径产生：核对原AdbPlan、原VoiceCapture与当前AT的实际Device，核对现用ADB descriptor/interface，通过同一个handle读取boot ID、PID/start ticks、完整NUL参数和D4/UAC路由状态；显式release interface并drop真实handle后才接受观察。原root FD绑定的ADB锁逐次wire、读后及publication验证。有限预算覆盖准入与wire，不宣称OS open/release/fsync硬实时上限；必要清理允许跨预算结束，取消不提前释放permit或原voice pin。

固定只读命令禁止glob，保留二进制argv边界；/proc/stat按comm右括号之后的字段解析，兼容空格及括号。正常读取/释放失败记录Unknown，真实owner/root/namespace/设备关系失效拒绝发布。serial、缺原capture等保持明确unbound/unknown，不选择新设备兜底。

这是原进程观察，不是恢复执行权、跨进程安全kill证明或原子远端快照。尚未实现独立有限physical RecoveryRight、实际恢复Plan/STOP或回读结算、独立RecoveryJournal/Receipt和完整原因覆盖后的解封；Unknown不自动重放。

## 实际验证

- 冻结2929项应用输入，较586改变12项、新增2文件；全部输入持续零漂移。
- 定向159项通过。完整daemon **2858通过、0失败、2个进程helper不计单测通过**。与586完整用例名比对净增10项、无删除；两helper另通过8阶段/16个独立死亡与重启进程。
- platform-fs 8项、API常规9项及独立DTO专项1项、独立进程集成10项通过；四个生产Swift测试程序执行通过，复用已验证且源码未变的581工具。
- 294 Rust/0 Go、450个审阅scope、129个Swift HTTP及2个非HTTP调用的源码门通过；当前独立Go trap未调用。
- Release后端新编译；GUI源码与data未变，复用并核验581 GUI payload。arm64包、ad-hoc严格签名、只含switchd-rs与libusb的后端payload和测试hook缺席均通过。后端SHA `19a9f49d0a475ff39cbeb0bd494337903113700f8b4006d4ff09a7b2988d7a4a`。
- 包内17场景23进程、三种日志管道退出、真实Swift文件→Rust DTO→daemon重启的3场景8进程、真实main的2场景2进程，以及10种正常Release EVS/WB向量通过。

Native实际claim/release、原远端helper、卡片业务没有实机通过证据。测试边界包括生产parser/registry、真实文件锁与actor的空registry路径，以及明确的blocking I/O替身；没有伪造Native Device证明物理成功。

首次编译失败保留：两处MutexGuard借用写法、三处旧测试Intent夹具缺字段。修正为局部guard并补None，未删除或放宽原断言，重新冻结v2再编译/测试。准备阶段发现复制的旧Go trap固定写旧marker，已在任何本轮构建/测试前修正；ledger检查器首次错误要求N/A栏有条件正文，改为核验其明确理由，文档未为检查器变更。汇总脚本也保留两次发布前schema误判：将向量明细/清理错误列表误当标量，现按原列表长度/空列表核验；未更改原结果或重跑测试。原失败和历史记录均保留。

## 实卡及进程交接

旧PID4114已不存在；580记录已完成当时强退及恢复安装应用。本轮只核对当前进程，未发送信号、未读写安装数据或替换安装文件。只读PCSC存在性检查确认唯一ESTKme-RED，PRESENT+MUTE、ATR长度0；释放context完成，没有打开卡会话、APDU、reset、IMS注册、发短信、拨号或改Profile。已请用户插稳卡并给读卡器断电重插，实际SIM World注册/被动收信仍待验收。

## 整体台账与清理

新增[叶子验收台账候选v1](acceptance-ledger-2026-10-19.md)及[JSON适用矩阵](acceptance-ledger-2026-10-19.json)：115候选叶子、49个不计分父分类、21场景类，10项范围/政策待审。零新增产品PASS，五项只索引586局部软件证据，其余NOT_REVIEWED不表示功能未实现。未测不妨碍明确要求入表，仍须完成性和适用性审定后才发布正式N。

所有构建/进程验证结束后，仅按cleaner白名单删除 `build/rust-target` 与 `build/swift-cache`。回收分配空间 **1701576704字节（1.58GiB）**，同期可用磁盘变化1486127104字节；源码、离线依赖、EVS、日志、所有历史失败/基线/运行数据、候选Products及正式dist保留并核验。没有生成DMG、没有安装候选。

证据：[最终门禁](../../../../build/reports/iteration-587-gate-summary.json)、[当前系统读卡器观测](../../../../build/reports/iteration-587-pcsc-presence-observation.json)、[首次编译失败](../../../../build/reports/iteration-587-final-v1-rust-compile-result.json)、[台账静态审议](../../../../build/reports/iteration-587-acceptance-ledger-static-review.json)。前序：[586](iteration-586-startup-metadata-foundation.md)。
