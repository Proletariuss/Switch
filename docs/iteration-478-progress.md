# Iteration 478：Rust 接管模块语音路由完整停止与清理流程

本轮将挂断、退出、还原及普通停止调用点的模块语音路由状态预检、停止、异常后的状态核验和真实音频回滚合并为 Rust 持有的完整工作流程。新增生产所有权计 P2B **+0.025**，P2B 为 **5.025/8.0（62.81%）**，整体为 **18.195/47.5 = 38.31%（估算误差约 ±3 个百分点）**。计分仅对应这组完整调用流程的接管；测试扩展、生产代次修复和 guard 补强不重复计入 P5 或其他项目。全 Rust 后端目标仍在实施。

## 生产流程及调用语义

沿用内部 `voice-route/stop` RPC，新增可选 `stop_policy`：缺省为 `always`，其余策略为 `hangup`、`shutdown` 和 `restore`。策略只用于停止请求；显式 null、未知值、用于其他操作的策略及不匹配的操作凭据均被拒绝。Go 调用点只提交一次请求，不再组合状态 RPC 与停止 RPC，也不在丢失响应后查询、重放或回退到 Go 清理。

Rust 按调用点保留原有错误语义：普通停止始终尝试停止；挂断时预检不确定仍尝试停止，清理成功即可返回；退出时预检错误保留，即使后续停止成功也不能消除该错误；还原时预检错误立即终止，不继续修改模块配置。已确认路由不活动的策略预检不获取 USB 语音租约，不关闭或重新打开 AT transport。还原失败继续区分状态确认阶段与停止阶段，以保留原有中文摘要。

停止响应丢失或返回失败时，Rust 保留原始错误，核验 D4 helper 是否真正退出，再关闭 `audio_enable` 并完成 T/T/B 音频回滚，最后核验实际路由状态。不能仅凭 helper 消失接受未完成的回滚；回滚或后续状态核验失败时保留原始错误及清理诊断。所有实际 ADB shell 前后均核验 producer，过期 worker 不继续发送命令，也不能发布成功。

## 并发、物理清理与退出

独立 Rust owner 持有整个预检、USB 租约获取、阻塞 ADB 工作及 `release_confirmed()`。ADB admission permit、ADB interface guard 和通话模式 operation guard 均保留至实际清理落定；HTTP 观察者取消或超时不释放仍在使用的资源，也不取消已接受的 worker。

还原携带当前 call-mode reservation 的 token 和 sequence，借用真实原生独占 guard，避免在自身独占锁下再次申请共享锁。普通和公开音频启停使用共享 operation guard；新增 `SharedVoiceGuard` 将这些短期 ADB transition 纳入原生 radio job 计数，在其存续期间排除冲突配置和物理 radio 工作。producer retirement 与 shutdown 等待该 guard 实际 Drop，不以逻辑 USB 租约消失代替物理清理完成；该 pin 只覆盖 transition，不持有整段持续通话。

公开音频启动将就绪检查时的 producer 与检查结果、共享锁一并传递，完成检查后及启动前再次核验，避免将旧 producer 的就绪结果重新绑定到替换后的后端。

Go 退出协调等待既有通话模式配置操作结束后再请求普通共享停止，不绕过或清空独占 reservation；共同退出时限耗尽时保留“退出未确认”，不据此退休仍可能发布事件的 producer。Go mutex 获取本身仍不可取消，RPC 时限仍只限制观察者；外层退出协调、配置生命周期和真实 worker 退出仍需继续收敛到原生 owner。本轮不能据此宣称全部 Go 通话模式生命周期已退出。

## 离线验证与独立审查

完整迁移门禁：**通过**。`build/reports/iteration-478-migration-gates.log` 以 `migration gates passed` 结束。按 Rust workspace 测试段汇总为 **2,174 passed / 0 failed / 6 ignored**，其中 daemon **1,538 passed / 0 failed / 3 ignored**；日志后续差分专项复跑不重复计入 workspace 总数。Go 全仓 test/vet、Swift BLE **38 组**虚拟事件回归、**2,726 项** carrier replay 及 USB/radio/HTTP/HEAD/IMS/SDP/AMR/MESSAGE/USSD 差分通过。

首次完整门禁在 Swift 编译阶段遇到 alias/module 生成缓存故障，证据保留在 `build/reports/iteration-478-migration-gates-before-cache-reset.log`；清理生成缓存后重新运行，完整门禁通过。该缓存处置未新增生产迁移信用。

专项证据：完整门禁中 Rust ADB 模块 **61 项通过**、module-radio 模块 **73 项通过**；Go 模块语音停止定向 race **24 个顶层用例通过**，日志为 `build/reports/iteration-478-go-voice-race.log`。真实还原 Stop guard 补测及停止策略/还原凭据的 HTTP wire 拒绝矩阵已纳入完整门禁；专项复跑数量不重复累加为 workspace 总数。

专项覆盖策略边界、不同调用点的错误保留、真实停止和 T/T/B 回滚、丢失停止响应后的状态恢复、回滚或状态失败、每条 shell 后 producer 过期、非活动路由不获取 USB 租约、观察者取消时保持 admission 与 guard、worker panic 后仍执行清理，以及原生 shared job pin 的 retirement/shutdown 排他边界。新增真实还原 reservation 的 Stop guard 用例，核验错误 token/sequence 与过期 producer 被拒绝、自身独占锁不冲突，以及清理 guard 存续时无法释放 reservation。

独立代码审查未发现当前合并停止流程的阻断问题。真实独占还原 Stop guard、停止策略及还原凭据 HTTP wire 拒绝矩阵，以及取消期间保留完整 native job/operation guard 的覆盖已补齐；这些测试补强不新增生产迁移计分。

契约 `--diff` 已报告通过：**354** 个 method/path（Rust **295** / Go **59**，含 **51** 个 HEAD alias）、**127** 个 Swift HTTP 调用点及 **381** 个审阅范围。Go 结构退出审计仍有 **6 类阻塞**、**265 个生产 Go 文件**，Rust gateway 和 App 构建、打包仍依赖 Go 后端。证据为 `build/reports/iteration-478-contract-inventory.log`、`iteration-478-contract-diff.log` 和 `iteration-478-go-exit-audit.log`。

## 下一完整迁移块：原生通话模式生命周期 owner

下一块应整体接管权威状态与刷新、启用、还原、重试及下载完成后的部署收尾，而不是继续增加 Go 调用的新包装接口。当前 Go 仍持有 `call_mode.go`、`call_mode_backup.go`、状态/重试/下载 adapter 及 `module_voice_*` 中的外层操作、状态缓存、超时、配置与重连编排；仅移动其中一个函数体会继续留下 Go 状态和 callback owner。

可复用的原生能力包括 module-radio 的 USB composition、IMS/VoLTE、ADB 授权及 reboot actions，Rust 下载与完整性校验、备份存储和 binding 验证，以及本轮完成的准备和 quiesce pipeline。整体退出还需补齐同一 reservation 下的原生模块身份查询（CGSN 与尽力读取 ATI）、USB/serial 两条 transport 的重新绑定与有界重连，以及重连后的 IMEI、完整 USB 配置、IMS/VoLTE 和真实 ADB root 验证。还需由原生生命周期 owner 独占维护 operation admission、状态、runtime generation、下载完成到准备的交接、取消与真实 worker 退出。

实施所有权可按三个完整职责安排：生命周期 owner 负责原生 reservation、身份/重连及 enable/restore job；状态与下载 owner 负责统一状态、refresh/retry 和 download→prepare 收尾；Go 退出工作负责删除旧 coordinator、字段、client、callback 注册及对应适配测试。预估可退出约 **1,300–1,800 行生产 Go 编排**；该估算需在 USB/serial 重连依赖和共享状态调用点清点后确认，不能提前作为已完成计分。

## Switch 3.18 发行与实机边界

按用户要求已生成 [Switch 3.18 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.18-macOS27.dmg)，build 318，arm64，最低 macOS 27.0，包含 Iteration 478。大小 53,329,757 字节，SHA-256：`d5d0bab64a111056991d726160165c52ad9a5fad8285275b068cf77d1b59f09a`。旧 3.17/build317 已验证包保留，仅覆盖 Iteration 477。

版本文件、Xcode 设置与 Rust workspace 已同步 3.18 / 318 / 3.18.0；外部依赖锁定版本未变。Release 构建成功；App 与全部嵌套 Mach-O 严格签名核验、镜像完整性、只读挂载、包内版本/架构/资源、实际动态加载依赖、13 份许可文件和归档哈希一致性均通过。补齐现有 uicc-go 与 ios-carrier-profiles 的随包许可全文，并核对与源码一致。dist/Switch.app 已同步为镜像内同一二进制，镜像已卸载。本机 ad-hoc 签名，未做 Developer ID 公证。

发行前已修正实际停止工作完成后的过期 producer 错误阶段：restore 在该处报告 stop，start/status 保持无阶段字段；同步契约指纹后，以 3.18.0 重新运行完整门禁并通过。包内依赖校验区分 dylib 自身 LC_ID_DYLIB 与实际 LC_LOAD_* 依赖；后者无 Homebrew 或 /usr/local 绝对依赖。

本轮未操作真实读卡器或号卡，未发送短信、拨号或执行设备与运营商网络验收。离线测试与代码审查不能替代实机验证；9eSIM `6982` 继续排除在本轮范围之外。Go 仍是构建与运行依赖，完整 Rust 迁移目标保持进行中。

发行证据：build/reports/release-3.18-version.log、release-3.18-build.log、release-3.18-verify.log、release-3.18-verification.json、release-3.18-source-files.json；发行清单位于 dist/release-manifest.json。所有证据路径均相对 Switch 根目录。
