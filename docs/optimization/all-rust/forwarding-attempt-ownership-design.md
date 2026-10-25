# 转发尝试归属与同进程人工重试：下一批实施决定

2026-10-23，承接已批准的稳定性目标。工作区仅为 Switch_MAC。
本文件确定下一批实施边界，尚未修改生产转发库，未授予功能验收通过。
P0–P6 整体验收仍开放。S07 当前冻结回归期间不混入此修改。

## 证据与问题

695 的 `build/reports/stability-695/unknown-retry-harness/result.json` 使用当前
Cargo 选择的 forwarding rlib、真实 Worker/QueueStore 和只接受 CONNECT 的本地
mock，复现取消型 Unknown：首次 durable submitting 后取消，自动 flush 不重发；
同一 Store 人工 retry 持久成功，随后却返回 InvalidTelegramCheckpoint 并再次
变为 Unknown。重开同一文件后人工 retry 能抵达下一次本地 CONNECT。
这证明当前 volatile chunk 标记妨碍同进程人工重试，不证明真实 Telegram
投递失败都源于此处。typed provider Unknown 分支尚需独立回归。

另有静态并发线索：AttemptReservation::Drop 锁忙时只按 task/channel 异步
release，未带尝试身份。旧 release 若晚于新 begin，可能删除新 owner。
该线索尚未运行复现；不能只删 chunk map 解锁而扩大此风险。

## 最小修改

1. 每个 QueueStore 实例持独立身份与 checked 单调 attempt sequence。
   只有已确认 durable begin 才返回 opaque AttemptToken，含实例与序号；不能构造、序列化为
   对外授权或在重开 Store 后复用。计数耗尽拒绝新 begin，不循环、不清账本。
   实例身份可由仅进程内 Arc 身份持有，匹配必须同时核对 task/channel/sequence。
   checked 序号分配/耗尽校验先于 durable begin，成功提交后才安装 owner 并返回 token。
   现有 reserve_destination 只有 volatile 准入，且当前仅测试调用，不能签发等权 token；
   首选删除该未使用生产口并改原测试验证 durable begin。若确需保留，必须使用不能进入
   checkpoint/finish 的不同 reservation 类型，并保持 Unknown/permanent 门禁。
2. in_flight 从仅有 key 的集合变成带 token 的归属表。生产 Worker 的
   checkpoint、finish、Unknown release 和 Drop release 都必须使用同一个
   token。旧 token、错 channel、错 task、另一 Store 的 token 不修改当前 owner。
   Rust 库是本项目内部接口，可按编译器完整更新调用点和原测试；不能保留一个
   无 token 的兼容写口绕过正在生效的归属保护。HTTP 与 SWF1 数据格式不改。
3. 人工 retry 仍先验证当前 binding、enabled、非 InFlight、非 poisoned、
   可重试业务状态，再持久提交现有 Retry 事务。只有该事务成功后，清除这个
   Telegram task 已退休尝试的 volatile submitting 标记。BeforeCommit 和
   AfterCommit 错误均不返回成功；后者继续要求 reopen。frozen chunks、digest、
   confirmed cursor、attempt count 和原 durable Unknown 规则保持。
   跨层同时修正 execute_queue_action 对 PersistenceUncertain 的回执：当前 AfterCommit
   会落入统一409，被 public_mutation_receipt 误写为 completed；改为明确不确定标志/相应
   server failure，使原 client_request_id 保持 unknown。已 poisoned 的 ReopenRequired
   当前会先由同一 queue guard 内的 inspect 以503拒绝，不将该路径误报为已证实409；
   后续重构需保留这个边界，区分已知未执行和提交后不确定。重开后的 pending 可能已包含
   先前人工授权，这是提交不确定而非自动新授权，不能回复“未执行”或重放该动作。
4. 自动 flush/notify/runner 重启不视为人工授权，不清 Unknown、不重发模糊结果。
   显式 retry 可能重复供应商已接收但丢失响应的通知，不能承诺 exactly-once。
5. token 保护先于 S12 磁盘隔离落地。后续 offload 必须让完整命令持有 flush
   准入、token、真实 IO completion 和释放确认；调用者取消不能让 late begin
   成为无人持有的授权。Drop 的异步清理需逐步纳入 retained drain，不把
   JoinHandle 不可见或 observer 超时当作清理完成。不能把包含 provider await
   的整个未来塞进 blocking 线程。
   本批同时完成窄范围的 release completion/drain：Drop 必须在派生清理前同步登记，
   完成/异常有真实归属，Configured.stop/retire 在确认这些任务结束前保留 Store/flock。
   当前只 join runner 不足以证明旧 Store 已退休；token 本身也不能使 successor open
   不再 Busy。不得持 queue guard await drain；错误/无法确认时继续保留退休错误围栏。
   stop 先禁止新producer并join runner，确认所有Drop已登记后再drain；不能在仍可新登记
   时观察一次pending=0就报告退休。完成计数/通知须晚于真实queue guard和清理closure
   自持Store Arc的释放，避免signal之后仍有短窗flock。完成barrier后立即同路径reopen
   必须成功，单独断言pending=0不足以证明该顺序。

若发现还有生产调用点无法可靠取得并保留 token，先补完整 owner 边界，不能用
无条件 clear/release 或全局放宽 checkpoint 代替。

## 必需软件验证

- 先保留当前失败回归，再验证同一 Store 的取消型与 typed Unknown 两条路径。
  每条都确认：无人工授权时 provider 新请求为0；人工 retry 后才允许一次新尝试。
  单 chunk 与多 chunk 分别测试；已确认 chunk 不再次发送，payload digest 不改。
- 真实 AttemptReservation 的锁忙 Drop：在旧 token 退出后，于同一持锁窗口人工
  retry 并建立新 token，再让旧 release 执行。新 owner 必须仍有效，旧 ACK、
  finish、release 不得改变它。组件交错与完整 Worker 交错分别标明。
- InFlight retry、错误 binding、禁用 channel、cancelled/terminal、poisoned、
  retry BeforeCommit/AfterCommit，均保留原拒绝与 Unknown fence；AfterCommit 必须
  经过实际 HTTP queue-action + public receipt + reopen 检查，不能只断言库返回 Err。
- Store 重开与 foreign token、不同 task/channel、attempt counter 边界；
  restart 保持 Unknown，旧 token 不能在新 Store 上取得权限。
- 锁忙取消后真实release尚未结束时，Configured stop/retire 不得成功返回；释放屏障后
  drain完成、旧Store/flock真实释放，同路径重开成功。失败/任务panic/无runtime不能假报
  完成。纯取消尚保有InFlight时人工retry应拒绝；late-release完整交错须经过typed Unknown
  的真实显式release节点，不能在测试中任意清owner后宣称完整链复现。
- Telegram/Bark 正常成功、明确失败/退避、原持久窗口和 caller/runner stop
  测试全部保留；原测试名称差集、执行结果与源码指纹分别记录。
- 本地 mock 及 synthetic 数据；不修改系统代理、不解析/连接供应商上游，不使用
  真实 token，不发送真实通知。最终全库与 daemon 回归、来源绑定另行完成。

该批只修归属与人工重试，不同时改队列密文、密钥迁移、历史退役、业务限额、
代理选路或全套 IO 执行模型。S10/S11/S12 其他设计各自保留验收边界。
