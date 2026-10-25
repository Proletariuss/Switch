# GitHub 最后本地同步源码审查（2026-10-25）

## 审查边界

- 发行基线：`v3.51` / `45c4eae4895d1fba7173bc1f3d0c0ab587cf6d5c`。
- 最后一次本地源码同步：`9b6a7701f02fe1059a385764ac621d4c87f490c9`。
- 本文审查的是上述两点之间的生产源码差异，以及新增生产 helper；测试结果必须由
  GitHub 托管 Runner 另行给出。
- 同步后的源码状态仍为 `source-not-final-G0B`。本文不继承已发布 DMG 的验收结论，
  不授予硬件、运营商、外部投递、通话音频或长期运行 PASS。

## 契约变化

HTTP census 从 306 增加到 312 个 method/path 契约，Rust owner 从 306 增加到
312，Go owner 保持 0；HEAD alias 从 57 增加到 59，Swift HTTP 调用点从 128
增加到 130。新增项仅为：

- `GET|HEAD|POST /api/reader/vowifi/stop-control`
- `GET|HEAD|POST /api/vowifi/stop-control`

GET 只读取当前 exact-owner 状态，POST 最多发布一次停止信号；HEAD 继承 GET
所有权但不增加 handler。请求必须为空 body，并携带唯一规范化的 scope、daemon
epoch、owner ID 和非零 generation。旧 owner、重复字段、别名编码、未知字段、非规范
generation 及不确定 body 均 fail closed。所有成功和拒绝响应都设置 `Cache-Control:
no-store`。临时 stop-control 不替代持久请求台账，也不伪造 join、资源清理或重启后
持续停止的证明。

## 24 个既有指纹的语义复审

### App 边界

- `BluetoothReaderManager.swift`、`SMSStore.swift`、`MultiStandbyStore.swift` 和
  `SMSForwardingStore.swift` 在隔离验收模式禁止蓝牙、Keychain、系统通知和外部转发
  副作用；普通启动路径不进入该分支。
- `BackendProcess.swift` 只在经验证的隔离验收上下文使用受控 data root 和 daemon
  capsule；普通运行仍由既有 Rust daemon 参数和本地认证启动。
- `CallRuntimeStore.swift` 将模块状态交给现有 module-status owner，而不是直接覆盖
  公共通话投影，避免模块轮询冒充当前 reader 会话。

### daemon 生命周期与 HTTP

- `main.rs` 新增 pre-entry capsule 拒绝、三类连接池统一准入/回收、共享完整响应
  writer，以及两个 stop-control 精确路由。public、callback 和 internal 仍保留独立
  上限和日志身份；退出时统一 abort 后 drain，不能把任务已取消当作业务清理成功。
- `ownership.rs`、`public_http.rs` 与 `request_body_budget.rs::body_limit` 对新增路由的
  owner、GET/POST/HEAD policy 和有界空 body 约束一致；未知方法没有获得兼容转发。
- `daemon_args.rs` 在历史 flag parser 之前扫描唯一 capsule，阻止通过 positional、
  terminator 或 flag value 隐藏验收参数。无 capsule 的普通参数语义保持原状。

### VoWiFi exact-owner 停止

- reader 与 module service 为每个运行记录绑定独立 stop owner；新运行替换旧记录时仅
  保留已实际开始停止的最后 owner 投影。
- POST 先在同步临界区保留一次性停止资格，再发布原 owner 信号；可取消的 HTTP
  waiter 不持有真实 join handle。只有原 worker join 和 claim/resource cleanup 均确认
  才报告完整清理。
- stale、poison、signal panic、join panic、未确认 claim 及 worker failure 均保持隔离，
  不释放 busy fence，也不自动启动替代会话。

### 短信转发与持久化

- `device_sms_forwarding.rs` 只允许空 handoff ID 的迁移台账先做 baseline；已绑定为
  其他 ID 仍在读 inbox 和 enqueue 前拒绝。损坏或语义无效台账 fail closed。
- `sms_forwarding.rs` 把 queue inspection 移到现有异步 queue-I/O owner，并在配置退出
  时先关闭 worker admission，再等待保留任务；取消观察者不能重新开放提交。
- `proletarius-forwarding::{queue_store,lib}.rs` 和 `proletarius-platform-fs::lib.rs` 的
  本轮变化主要是条件测试钩子及 process-start identity 接线；默认生产 feature 不启用
  queue fault 注入。真实 worker/queue-I/O 和 identity 子模块另设独立指纹。

## 新增生产 helper 的独立保护

下列文件不以父文件指纹替代审查，均加入 `api-contract-review.json` 的完整生产源码
指纹：

- Swift acceptance context/entry，以及 module/reader stop-control 调用和 DTO；
- daemon acceptance capsule、connection pool、HTTP response writer；
- reader/module stop-control owner 和 service adapters；
- forwarding worker queue-I/O、process identity、QMI write gate、legacy-empty store。

测试模块由 checker 的生产源码剥离规则排除；生产 helper 的改变必须重新进行本节同等
级别的人工复审。

## 待验证风险与云端门禁

Swift 的 fallback stop 目前先等待临时 stop-control，再恢复原持久 request ID。临时
GET 连续网络失败或长期 pending 是否会延后持久回执恢复，必须由定向 Swift 回归测试
判断；在得到证据前保留为可用性风险，不宣称缺陷已关闭。

本基线合并前，GitHub 托管 Runner 至少必须通过：

1. source preflight、契约 census、Rustfmt 和两份 Cargo metadata；
2. Rust workspace 非 daemon 测试；
3. daemon 除六个共享串口用例以外的完整软件测试；
4. 六个共享串口用例逐例、独立进程、单线程执行；
5. daemon capsule 独立 workspace 的 fmt、clippy 和 test；
6. App 相关改动的 `Switch.app` 构建。

故障注入测试不是实际只读挂载、ENOSPC、真实设备或运营商验收。macOS 26 GitHub
Runner 上的成功构建也不等于 macOS 27 真机验收。
