# Iteration 322：读卡器业务准入与原子所有权交接迁入 Rust

2026-10-10。按用户“输出 DMG 后再继续推进”的要求，2.96 阶段包已经交付；本轮只更新源码，以下改动不在该已冻结安装包中。

## 已切换的生产路径

Rust `reader_reservation::Gate` 在同一把锁内持有普通操作、原生会话、N 待的业务 capability 与物理 reservation。私有 `POST /api/internal/reader-business` 处理 acquire、transfer、release 和 status；请求受内部凭证、4 KiB 上限、严格字段解析和所属 Go producer 限制。

- 每次准入使用明确的 producer、token、递增 sequence 和 role。重试只重复同一准入请求，不重放 APDU、下载或其他卡片命令。sequence 高水位阻止已经释放、被拒绝或延迟到达的 acquire 再次获得占用；无需用固定 TTL 撤销长时操作。
- 普通 API 和后台 eSIM 下载显式携带 capability。operation → native_session 在同一 Rust 临界区交接；旧 operation 的 defer 不能释放已经转交的 native/N 待 owner。
- N 待 inventory 从 operation 原子转为绑定 reader/generation 的 multistandby；正常启动同时获得业务和物理 reservation。退出恢复期间真实 worker 计数未归零时不得释放。transfer 回应丢失时，Go 只保存两个精确角色的待清理 capability，Rust 仍是权威 owner。
- native 租约释放、过期、PC/SC 退出和 transfer 结果不确定时，必须确认物理释放后才释放业务 capability；失败保留租约和 `authMu`。补上完整 Go 回归发现的 `ClosePCSC` 清理遗漏。
- status 缺字段、null、错误类型或 IPC 不可用时失败关闭。已有普通 VoWiFi/live-session 检查仍在取得 capability 后执行，不能用这个短期准入代替真实 IMS/VoWiFi 生命周期。
- Go child 仅在 supervisor 确认退出后退休 producer；Rust 在途物理 worker 和自身 BLE 控制许可继续保持至实际结束。

公开 BLE scan/connect/disconnect 直接使用 Rust 全局门禁与 BLE I/O permit。持有许可的独立任务在 HTTP 调用者取消后继续等桥接回应或既有超时。删除 Go BLE compatibility lease endpoint、token/expiry RPC、三个 Go 全局 busy 标志及旧无参 begin/end 方法；删除无人使用的 N 待 physical-only acquire/release endpoint 和 Go helper。AST 回归禁止这些权威状态重新出现。

N 待存储故障仍在新的 acquire/transfer 前阻止启动；status/release 和退出恢复保持可用，不因持久化故障而遗失清理机会。

## 交叉审议与验证

三个并行范围分别检查并实现 Go API/native 租约、N 待及观察者、Rust BLE 控制退役；主任务实现 Rust Gate、producer 监督接线、故障场景和接口清单。独立审查复核 capability 的 sequence、角色、代次及取消语义。测试发现并修复 PC/SC shutdown 遗漏 business release；readiness 固定基线同步记录 BLE 控制已无 Go 依赖。

- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,765 passed / 6 ignored**（daemon **1,236 / 3 ignored**），Go vet/全包测试及 carrier、USB、radio、HTTP method/HEAD、codec、SDP、IMS MESSAGE 差分通过。
- Rust reader reservation 定向 **22 项通过**；原生 BLE control 定向 **7 项通过**。Go 新业务准入、native 租约、N 待交接/丢 ACK/物理释放故障定向测试通过；相关 Go `-race` 检测通过。
- 契约清单 **335 method/path（Rust 275 / Go 60）**、127 个 Swift HTTP 调用点、334 个审阅范围；净减少两条接口是删除三个兼容入口并增加一个 Rust business 入口，不是功能减少。扫描器自身 **43 项通过**。
- `git diff --check` 通过。使用 Switch/build 内临时 Go shim 调用本机 Go 1.26.7，门禁后已清理；没有改变全局工具链配置。

完整门禁、并发检测、契约与扫描器日志位于 `Switch/build/optimization/2026-10-10-iteration-322-*`。先前失败的 daemon 日志保留用于追溯 readiness 组件基线更新；最终结论以 `iteration-322-migration-gates.log` 的完整通过记录为准。

## 保留边界与下一步

Go 仍持有 reader 选择与 generation、`authMu`、多命令事务协调、native 物理租约适配和 BLE watchdog，以及真实 VoWiFi/N 待会话生命周期。普通短操作 release 在持续 IPC 故障下最多同步重试三次后记录未确认并保持 Rust 闭锁；本轮未增加后台补偿队列，确认所属 Go producer 退出后才能撤销这类残留。

此轮没有操作实卡、发送短信、拨打电话或连接运营商。整体加权进度仍为 **31.4%**，不把接口数、测试数或粗粒度组件比例当作总体完成率。readiness 的独立组件清单现为 8/21 个无 Go 依赖组件（38%），仍有 12 个 Go 阻断组件，`ready_to_remove_go=false`。

下一步接管 reader 的选择/generation 与物理事务生命周期，逐步移除 Go `authMu` 和 watchdog；需要同一读卡器 actor 的全部消费者切换及真实设备验收，不能把本轮准入切换视为完整 reader owner 或全 Rust 完成。
