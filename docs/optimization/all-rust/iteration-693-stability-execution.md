# Iteration 693：稳定性目标首批实施与定向软件验证

用户已于2026-10-23批准[持续目标](stability-goal-2026-10-23.md)。本批修改当前
`3.50/build350` 工作树，未构建正式 App 或 DMG，也未替换已安装应用。
以下是定向软件证据，不授予任何完整 P0–P6 阶段 PASS。

## 实施与复核

- S01：本机 HTTP 连接由 I/O owner 唯一关闭；取消只标记并 shutdown，连接/读写共享
  总期限，禁止 SIGPIPE，响应头/正文/chunk/trailer 有界。新增独立回归并接入主测试脚本。
- S02：首次/legacy 转发配置的完整快照、队列密钥、意图先提交并读回，之后才发布；
  已有加密队列缺原 key 时阻断，不生成替代 key。新生成密钥推进旧意图以免身份冲突。
- S03：音频启动使用调用方预先持有的 ticket，资源安装在同一音频队列完成；权限回调、
  取消和停止绑定原 ticket，旧任务不能复活或关闭新通话。未修改编解码/音质算法。
- S04：eSIM 已耐久 admit 但未 claim 的 prepared/count=0，重启后变成未派发终态，
  原 ID 不再执行；已 claim 的请求仍保持 unknown，禁止后继及重放。
- S05 部分：逻辑通道关闭失败保持未确认，重复 close/transmit/Drop 不再发送 APDU；
  通知 list/retry 显式关闭，包含 SELECT 失败后的 typed cleanup 错误，停止 AID fanout。
- S06 部分：Reader 保存 stop 检查点后，重复关闭不生成新 ID 或覆盖原观察身份。
- S07 部分：BLE control、N待发送和消息归档三个 collector 使用5秒绝对收包期限，
  每64帧让出执行权；408仅表示尚未派发，已受理的长操作不受该期限取消。
- S09：媒体健康期限只在真正检查后推进，控制流量不再延后检查；terminal 状态移除
  媒体 watchdog，清理失败不形成过期计时忙循环，独立 SIP 事务期限继续保留。

执行代理 High 分工；领域实现后互审，主代理整合并检查关键链，第三轮再次审查修正。
审议曾发现并修正两项新缺口：terminal 媒体过期计时忙循环、通知打开失败降级继续探测。
自动 Keychain GC 的跨进程 staging/active 删除风险也在互审发现；本批移除了自动删除，
未采用只能覆盖单个 DataRoot 的局部锁来宣称共享 Keychain 已安全。

## 已执行证据

原始输出保留于 `build/reports/stability-693/`，音频执行收据保留于
`build/stability-audio/verification.json`。Rust 整合回归使用 `--locked --offline`。

| 组 | 结果 | 日志 |
| --- | --- | --- |
| eSIM intent | 14 passed | integrated-esim_mutation_intent.log |
| IMS calls/media | 192 passed | integrated-native_ims_calls.log |
| 通知相关 | 57 passed | integrated-notification.log |
| BLE control / N待发送 / 消息归档 | 8 / 7 / 35 passed | integrated-reader_ble_control.log / integrated-multistandby_send.log / integrated-message_archive.log |
| HTTP ingress | 7 passed | integrated-http_servertests.log；另有 s07-http-ingress-final.log |
| CCID 单元及集成 | 4 / 3 passed | ccid.log |
| VoWiFi 回执 | 14 passed | vowifi-operation-repaired.log |
| VoWiFi service（本地模拟 IKE/IMS） | 102 passed | vowifi-service.log |
| 转发单元及集成 / 系统代理 | 64 / 3 / 8 passed | forwarding-proxy.log |
| Swift | UnixSocket transport、API request policy、forwarding client、Reader audio recovery、CallAudioStore、playback/PCM、VoWiFi readiness、BackendReadiness、ReaderESIM identity 回归通过 | 部分见本目录日志；音频另有独立 JSON 收据 |

原 `vowifi-operation.log` 留有 13通过/1失败：旧测试把重启恢复后的 unknown 错误地
断言为原 starting，且用 recovered 条目测试本应跳过的 live observe 写盘失败。
仅修正测试，将磁盘字节回滚、重启 unknown、未恢复 live owner 的 observe 写盘失败
分别验证；生产恢复语义未放宽。第三轮只读审查确认这项拆分保留两种安全要求。

整合前后九个直接关联 Rust 源文件的散列在 `rust-before.sha256` /
`rust-after.sha256` 相同；随后只格式化旧 provider/ownership 断行并完成整合组重跑。
全 workspace Rust fmt、指定差异空白检查通过。全部执行记录只证明相应源码切面。

## 仍未闭合

- Module unknown-ID 安全核对、普通 VoWiFi 临时故障恢复和 N待清理后恢复。
- 其余 eSIM rename/switch/delete/download/overview 的类型错误保留和显式关闭，
  不能由通知回归推定全部 eSIM 路径通过。
- Keychain 跨 GUI/跨 DataRoot 的统一命名空间和互斥；并发 bootstrap 仍可能选择
  不同 key。历史快照暂留，GC 有界退役尚未实现。
- 五类操作账本达到容量后的安全退役、其余请求体 collector、消息缓存与 BLE 日志边界。
- 实际连续24/72小时、完整隔离冷构建和最终软件验收、最终三轮发布审议。

接口门禁 `contracts-pending.log` 当前为失败：新增
`POST /api/reader/vowifi/reconcile` 未同步库存，修改后的来源指纹未完成更新。
静态 census 为305个声明 Rust owner、0个 Go owner、127个Swift调用点；
这些数值只表示接口归属，不是整体功能完成率。本批未绕过检查或自动批准新指纹。

正式台账仍为49父项/115叶项/21场景类、0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；
分母未冻结，不计算产品完成百分比。实机、运营商、外部 Telegram/Bark、真实声音
及旧版回退保持未验收。项目目标继续 active。

## 缓存处理

所有编译停止后，lsof 检查两个目标目录没有打开文件，按白名单精确清理
`build/swift-cache`（423,600,128字节）及
`src/rust/target/debug/incremental`（8,031,752,192字节），合计
8,455,352,320字节，约7.875GiB。清理结果见 `cache-cleanup.json`；源码、依赖、
测试日志、产品与恢复资料保留。当前 `build/rust-target` 保留用于接续工作包验证。
