# Iteration 479：Rust 接管原生通话模式生命周期

本轮将通话模式启用、还原、权威状态与刷新、重试，以及下载完成后的运行时部署收尾接入原生 Rust 生命周期 owner。旧 Go 编排及 7 条私有适配路由已退役；radio reservation、producer、runtime generation 和 USB/serial session 共同约束实际执行与状态发布。本次发行收敛暂不新增迁移计分，维持已审计的 **18.195/47.5 = 38.31%（估算误差约 ±3 个百分点）**，待后续整体迁移验收核算。全 Rust 后端目标仍在实施，不能据此标记 100%。

## 原生业务流程与状态所有权

`call_mode_lifecycle` 持有操作准入、后台任务、权威状态、状态刷新与显式重试。`call_mode_state` 区分 operation generation 与非运行状态 observation revision：旧观察不能覆盖已开始的操作，旧 operation 或 producer 的完成结果不能发布到当前状态。公开 enable、restore、status、retry 和 download handler 完成既有请求、确认及 backup ID 校验后直接调用原生 service，不再通过 Go 私有 adapter 编排硬件流程。

启用在同一原生 reservation 中读取 USB 与 IMS/VoLTE、确认模块身份、保存保护备份、按确认条件提交 ADB 授权、仅修改目标功能位并回读，随后受控重启、重连和核验。同一 IMEI、完整 USB 配置、IMS/VoLTE 及真实 ADB root 通道均须确认；本地运行时文件通过校验后继续部署与语音路由启停自检，不能仅凭下载完成或接口配置将状态标记为已就绪。

还原使用原生备份存储与身份绑定验证，保留 schema 3 写入及旧 schema 的读取兼容性、完整 USB 还原命令、可选固件信息和不含 voice 的旧备份语义。IMEI 或 VID/PID 不匹配时停止；需修改时先保存当前配置作为保护备份，再写入、回读、重启和验证。持久 ADB 授权已被接受后的失败状态保留授权事实与既有中文说明，不将关闭 USB 接口等同于撤销授权。

状态观察在模块 VoWiFi 占有 SIM 通道、原生操作执行中，以及无需自动重查的失败或 needs 状态下保留当前快照。已就绪快照仍须受 runtime generation 与模块 boot ID 验证；应用或模块重启后，本地文件存在不能代替模块侧部署。显式重试由原生 owner 重新读取事实，返回仍需确认的下载状态，或启动已下载运行时的 prepare；下载仍须用户确认。

下载由 Rust 持有固定来源、完整清单及 SHA-256 校验，复用通过校验的缓存，并保持转发源尝试时的公开进度单调。下载准入要求原生观测处于 `needs_download`；完成后由同一生命周期任务交接至 prepare。producer/generation/cancel 检查覆盖网络读取、缓存、暂存和原子提交前的边界；过期暂存文件被清理，不覆盖原缓存。26 分钟观察期限仍保留，期限或取消不提前释放尚在清理的实际 worker。

## 真实清理、退休与会话边界

准入和已接受的任务由独立原生 owner 持有。HTTP 观察者取消不取消已经接受的实际任务，也不释放它持有的 radio reservation、operation lock、ADB admission 或 USB guard。`ReadPin` 将状态与就绪观察纳入实际排空；producer retirement 等待操作、读取及 shutdown gate 清理，不能只凭逻辑状态结束确认旧代已经退出。

退出请求捕获发起请求的 producer，并在取得 shutdown gate 后再次验证；旧请求不能改为停止替换后的 producer，也不能设置全局停止状态。radio 重绑定保留 reservation 身份与独占锁，USB 与 serial 均以当前 producer/session 校验接纳新连接。串口旧 session 的关闭及在途清理须落定，不能将旧关闭结果应用到新 session。配置结果不确定时停止后续写入，不通过重新绑定盲目重放配置。

Go 原有通话模式状态/cache、启用、还原、重试、下载准入及完成 callback 编排已移除。退役的 7 条私有路由为 `enable/start`、`restore/start`、`status/observe`、`retry/observe`、`retry/start`、`runtime/download/admit` 和 `runtime/download/progress`，均位于原 `/api/internal/call-mode/` 前缀下。保留的 Go 业务消费者使用原生生命周期接口，不再持有这组通话模式流程的权威状态。

## 离线验证与失败处置

完整迁移门禁通过：Rust workspace **2,202 passed / 0 failed / 6 ignored**，其中 daemon **1,566 passed / 0 failed / 3 ignored**；Go 全仓 test/vet、Swift BLE **38 组**虚拟事件回归、**2,726 项** carrier replay 及跨语言 USB/radio/HTTP/HEAD/IMS/SDP/AMR/MESSAGE/USSD 差分通过。Go 定向 race 通过；最终 ownership 专项 **33 项通过**。专项和差分复跑不重复计入 workspace 总数。

新增离线回归覆盖 operation 与 observation 栅栏、下载中代次失效及暂存清理、备份外部存储等价性、公开请求确认/null/未知字段/backup ID 校验、producer 退休与 `ReadPin` 排空、通知竞态、观察者取消后仍保留真实 owner，以及陈旧 shutdown 请求与 retirement 的 gate 边界。测试使用受控 fixture，没有提交实机操作。

首次完整门禁有 3 项失败，涉及 identity outcome 的 verified 标记及旧串口 close fixture 预期；修复后重新执行。第二次完整运行在 quiesce 取消测试的文件锁 Busy 即时断言失败。该测试独立重复 **20 次均通过**；保留实际 guard 与 retirement 断言，仅允许 Busy 在 **3 秒有界窗口**内确认释放，I/O 错误仍立即失败。完整门禁随后重跑通过。这些证据没有确认 fork 或其他特定根因，不能将推测写成已证实原因。

验证证据位于 Switch 根目录下的 `build/reports/`：

- `iteration-479-migration-gates.log`：最终完整门禁。
- `iteration-479-migration-gates-before-identity-and-close-fixture.log`：首次失败记录。
- `iteration-479-migration-gates-before-quiesce-lock-wait.log`：第二次失败记录。
- `iteration-479-quiesce-repeat.log`：quiesce 独立 20 次重复验证。
- `iteration-479-go-race.log`：Go 定向并发检测。
- `iteration-479-final-ownership-tests.log`：最终 33 项 ownership 专项。

## 契约盘点、剩余迁移与发行状态

当前 census 为 **351** 个 method/path：Rust **299**、Go **52**，包含 **51** 个 HEAD alias；Swift HTTP 调用点 **127**，审阅范围 **380**。Go 结构退出审计仍有 **6 类阻塞**、**259 个生产 Go 文件**。路由与文件计数只用于盘点，不换算为能力完成率，也不证明应用已经具备零 Go 构建或运行依赖。

本轮暂不新增 P2B、P5 或其他计分，维持累计 **38.31%（18.195/47.5）**；后续需按完整能力接管与整体迁移验收核算。普通离线门禁通过不能替代 Rust-only 退出门禁、干净无 Go 构建、设备行为及运营商网络验证。后续继续收敛仍保留的 Go 生命周期、业务消费者和构建/运行依赖。

按用户要求，已生成并验证 [Switch 3.19 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.19-macOS27.dmg)，build 319，源码截止 Iteration 479。大小 **53,366,723 字节**，SHA-256：`36c3c91370eb876b575e23ff69500482575c45e2cd88854df33766ea9f234356`。

Release 构建、App 与全部嵌套 Mach-O 严格签名、arm64 架构与最低 macOS 27.0、13 份许可文件、实际动态加载依赖、镜像完整性、只读挂载及归档 manifest 哈希核验均通过；实际加载依赖无 Homebrew 路径。`dist/Switch.app` 与镜像内 App 一致，镜像已卸载。构建前后 **2,796 个源码文件**的哈希保持一致。本机 ad-hoc 签名，未做 Developer ID 公证；发行校验没有执行实机操作。

发行证据为 Switch 根目录下的 `build/reports/release-3.19-build.log`、`release-3.19-verify.log`、`release-3.19-verification.json` 和 `release-3.19-source-files.json`。本次阶段发行包含原生生命周期接线，仍有 Go 构建与运行依赖。

本轮未操作真实读卡器或号卡，未发送短信、拨号或执行设备与运营商网络验收。9eSIM `6982` 继续排除在本轮范围之外。既有已验证发行包的历史记录保留；本轮阶段发行不代表完整 Rust 迁移完成。
