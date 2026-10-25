# Iteration 696：转发尝试归属、人工重试与释放确认

2026-10-23，3.50/build350。承接已批准的稳定性目标；唯一工作区仍为
`/Users/StatTrak/Desktop/Switch_MAC/`。主代理整合、子代理 High 执行。
本轮不生成 DMG，不接触实体设备，不投递真实 Telegram/Bark 消息。

## 已写入源码

- QueueStore 只在 durable begin 成功后签发进程内 AttemptToken。实例身份、
  checked 序号、task 和 channel 一起匹配；旧 token、错目标和重开前的 token
  不能确认或释放新尝试。序号耗尽拒绝准入，不清账本、不循环。
- Worker 的 begin、Telegram chunk 提交/确认、Telegram/Bark finish、Unknown
  和 Drop release 都携带原 token。移除未使用的无 token volatile reserve 口，
  不保留能够绕过归属核验的兼容写口；HTTP 和 SWF1 格式保持。
- 人工 retry 和 Telegram cancel 只有在原事务持久提交成功后才移除对应临时
  submitting 标记。自动 flush 不清 Unknown；BeforeCommit、AfterCommit、
  InFlight、绑定变化和 poison 保留各自拒绝边界。已确认 chunk 不重发。
- 锁忙 Drop 在派生 release 之前同步登记，最多64项；真实清理先释放 queue
  guard 和自持 QueueStore Arc，再发布完成。panic、取消、无 runtime、饱和
  或状态锁 poison 保留失败围栏，不能以零可见任务假报成功。
- Configured 独立保存 runner join 结果；停止等待者被取消后可继续等待同一
  drain，不重 poll 已消费的 join。新 flush 关闭后，已受理 owner 仍可登记清理。
  library run 也先检查已置 true 的 shutdown，并优先处理停止分支。
- queue-action 的真实 AfterCommit 返回503并标明 uncertain；外层公共回执
  保留原409 unknown，原 client_request_id 重放不再执行动作。已知 CAS 拒绝
  仍为 completed。重开后磁盘可能已有先前人工 retry，不能说历史未执行。

实现范围为 forwarding 库和 daemon 转发模块，3份 Cargo/feature 接线；没有
改代理策略、供应商超时、密钥迁移、队列加密或用户数据格式。

## 当前验证

证据目录为 `build/reports/stability-696/`，命令、时长与退出码分别存于
`validation-phases.json` 和 `validation-full.json`，不以文档代替运行结果。

- forwarding 库86项通过，独立进程队列锁3项通过；原64个库测试函数名称保留，
  增加7个 QueueStore 和15个 Worker 测试。
- daemon 转发40项、公共回执6项通过；各过滤组相互独立计数，不相加为全项目
  测试数量。定向范围包含4项新增 daemon 组合测试。
- 普通后端 `cargo check` 通过。实际 compiler-artifact 显示普通检查的依赖
  features只有 default；daemon测试依赖含 queue-fault-tests。此证据不等于
  Release/App 包故障符号审计。显式 all-features 仍会启用该测试特性。
- daemon 原3382个测试名称全部保留，新增4个，共3386个登记。完整回归最终
  3378通过、0失败、8忽略，耗时1841.49秒；精确执行名称与登记集合一致。
  8个专用进程helper在此调用中仍忽略，不计通过。此结果不授予整项目PASS。
- 709项 Rust源码、清单、toolchain、Cargo.lock和接入fixture已冻结，较695新增
  2个测试文件、修改8项、删除0项。所有编译测试阶段前后核对完整集合和哈希。
- 8份修改Rust文件格式检查及差异检查通过。709成员的部分Rust输入归档逐项
  核对hash；它不是完整项目或隔离冷构建胶囊。所有构建测试暂停后，仅清理白名单
  `build/rust-target/debug/incremental`，lsof确认无占用、cleaner进程保护通过，
  回收4,246,097,920分配字节（3.954GiB）。依赖、受测二进制、运行恢复数据、
  EVS、证据和当前发行产物保留。

测试使用隔离文件、synthetic 凭证和本地 mock。AfterCommit 经过真实 Hyper
duplex Incoming → Service queue-action → 实际加密 rename → 公共回执 → 重开，
没有经过 main 接入认证/预算。Unknown 的初始磁盘种子为合成数据；typed Unknown
和取消型 Unknown 的实际提交链由库 Worker 测试另行覆盖。daemon runner 清理
测试仅接受本地 CONNECT，不响应隧道、不解析或连接供应商上游。

## 仍开放

本轮 release drain 只确认 provider reservation 的派生清理。已存在的
enqueue_admitted/device_snapshot archive job 可在等待者取消后继续持 queue
guard；这属于S12的配置级IO owner/drain后续工作，不能由本轮结果推导所有旧
Store已经退休。同步磁盘隔离、公共回执异步化、S10安全退役、S11有界元数据、
S13同源完整冷验收和S14真实24h/72h长稳仍需实施与运行。

下一批只读设计已分别绑定源码，并形成root `s12-owner-refinement.md`：明确
promotion与close原子裁决、内部收尾复用父ticket、Preparation显式析构、已知
业务失败与资源未知分层，以及保留输入组合S07字节预算。设计和交叉反馈不计
实现通过；原始方案字节保留。优先推进配置级queue-I/O归属，再迁receipt流程。

实机、运营商、实体音频与外部投递分别未验收。正式台账保持49父项、115叶项、
21场景类，0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；P0–P6未闭合，整体计分分母
尚未冻结，不报告百分比。本轮静态交叉审查不替代最终三轮全项目审议。
