# Iteration 580：短信转发迁移快照的真实落盘与重启验证

日期：2026-10-19。P1–P4 同步验收，P5 完善，P6 最终验收未完成；294/294 仅表示后端接口接管。

## 实现

新增生产 `SMSForwardingCheckpoint`，将 Swift 一次性旧数据迁移快照保存为固定、不可覆盖的文件。保留原 UserDefaults 与 Keychain；旧冻结快照只读导入，业务转发与消息仍由 Rust 管理。初始化先由现有 BackendProcess 创建数据目录，再固定目录描述符，避免首次启动时永久固定到不存在的目录。

存储固定原目录句柄，以真实非阻塞 flock 序列化；校验同用户、普通文件、单硬链接、0600、nofollow 与 8MiB 上限。完整写入并同步临时文件后，以 Darwin RENAME_EXCL 提交，再同步目录；竞争时读取实际先提交的原文件。提交后目录同步失败保留文件，后续操作必须重新完成真实文件/目录同步，不把曾经存在视为确认。

Store 优先采用确认的原文件，其次未提交的原 pending bytes、只读旧快照、Rust 已接纳快照或首次源冻结。IO/结构/ID/ACK 错误均阻止 POST，不能轮换迁移 ID；坏文件不回退到另一份来源。原 ACK 与完整字段验证保留。

独立测试程序执行生产 Store/Checkpoint；只替换现有系统配置/Keychain外围与受控 HTTP 接口。真实后端测试使用窄 Unix HTTP 适配器，并不宣称完整 GUI 的网络栈或系统桥已验收。跨进程 writer/restart/Rust DTO/Swift verify 及真实 daemon 重启链的最终结果另见下方记录。

## 已保留的失败与修正

初版 Swift 的 Darwin.flock 名字解析到 C struct，编译失败；改为从公开 libSystem 精确解析 C ABI，解析失败真实报错。初版 Cargo 驱动使用了错误工作目录，随后修正。最终 Cargo 实际成功但 wrapper 误要求恰好两个产物：实际为 API、daemon 与三个合法 integration executable；保留原失败报告，并按独立目标身份核验实际产物，未把它当成编译失败。

普通客户端初次断言比较了从 Rust 恢复前后 JSON 的原始键顺序；改为当前冻结文件在下一操作前后逐字节不变，字段与迁移 ID 的严格比较不变。初次完整 daemon 2628 通过、1 失败：multi-thread 语音夹具在收到最后一帧 UDP 后立即读取尚未发布的 host 计数。测试共享有界统计屏障，核对同 generation/state/call/token/port/started_at 及四项精确计数；超量立即失败，负向零计数窗口保留，未改生产计数或产品超时。

## 最终验证

当前冻结 **2896项输入、较579变化14项**；第一次冻结、首次完整构建和最终仅测试屏障增量均保留。最终完整 daemon **2629 passed、0 failed、0 ignored**；API常规9项通过、所需ignored用例另在当前独立落盘链中精确执行1项通过；三个独立进程集成程序合计10项通过。17项原始文件测试、完整客户端、真实权限失败以及提交后目录同步失败的重试测试均通过。不是本轮重跑了整个Rust工作区或所有Swift suites。

跨语言门禁真实执行3场景、8进程：原生产文件落盘、独立Swift进程在源数据变化后仍读取原文件、当前Rust DTO精确测试、第三Swift再次打开原文件核对；另一全新目录中由Swift真实GET/POST到包内Rust后端，重启双方后以新token重放原快照，旧token401，两类409冲突且ledger原字节不变。`passed`和`full_cross_language_dto_roundtrip_passed`均为true；所需用例不再停留于缺少真实持久化前置的状态。见[完整持久化报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-580-handoff-durability.json)。

API/Go退出门禁通过：294个Rust接口、0个Go owner、54个HEAD别名、435个审阅范围、129个Swift HTTP及2个非HTTP调用。完成完整SwiftUI/Rust Release构建及测试修正后的再次构建；严格ad-hoc签名、arm64、许可、仅Rust/libusb后端，生产包无Swift或原生故障注入入口。实际包内17场景23进程、三类日志退出及正常EVS archive的10模式向量通过。候选ID为`com.proletarius.switch.candidate580`，不替换安装。后端SHA256：`d91c22df821feb666a4228ae75a696ad510ea74955f6e864427ec31e23fa02d4`。

## 实机与安装应用交接

用户先前指明的PID4114已经不存在。当前同一安装应用的后台PID13468在正常退出请求及等待后仍存活；按本次明确授权，核对路径、二进制hash和出生身份后强制退出。此强制退出不计作正常退出验收通过。未读取或删除旧用户数据，不对新候选执行安装。

当前候选在新的私有目录中以合成legacy隔离证据阻止非目标模块探测，实际选择唯一ESTKme-RED，只执行一次connect和一次必要的metadata refresh。连接返回200但无USIM身份，刷新返回502/reader_refresh_failed；没有进入EID/Profile扫描、VoWiFi注册、发信、拨号、切卡或下载。测试前后独立PC/SC均观察到PRESENT+MUTE、ATR0、in_use=false；这不足以确定底层硬件或驱动根因，也不是已确认的运营商注册失败。

实际候选断开确认、SIGTERM退出0、原进程组消失、合成隔离文件与源码/二进制均不变。原安装应用已恢复（本次恢复后GUI67895、后台67932），所有新runtime保留。已请求用户重新插拔读卡器和目标SIM World号卡后复测；本轮实卡业务验收仍失败/未完成。见[实际读卡结果](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-580-reader-read-observation.json)与[安装交接记录](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-580-installed-maintenance.json)。

## 缓存回收

所有编译/测试/硬件harness结束后，先逐路径确认无打开文件、无活动构建或测试，再按Ruby cleaner列出的7条精确路径删除。回收分配空间 **1,937,104,896字节**，同期可用空间增加 **1,733,292,032字节**；2896项源码输入和14,593项保留条目零漂移。原始失败记录、当前精确Rust/Swift程序、全部baseline/runtime、EVS archive与向量、Products及正式dist保留。见[清理报告](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-580-cache-cleanup.json)。

最终软件验证见[580软件摘要](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-580-final-software-result.json)，完整产品验收与项目目标仍未完成。

## 保留边界

新文件 checkpoint 的进程重启与真实 sync 证明不能追认旧 Foundation synchronize 已经成功；未访问真实用户旧偏好设置进行原位升级，也不替代 GUI/Keychain/provider、断电、完整升级回退或运营商验收。启动恢复权利、legacy 共享生命周期、物理设备绑定、多 Profile 长稳及当前候选实机业务仍按原方案保持待验收。不生成 DMG、不替换正式安装或 dist。
