# Iteration 092：USB AT 自动短信接收由 Rust 编排

日期：2026-10-05。全 Rust 后端迁移继续进行；所有源码、缓存、日志与合成测试数据都在 Switch 内。没有启动生产 App、访问真实读卡器或模块、收发真实短信、修改号卡或生成 DMG。语音目标仍为 **EVS 优先、AMR 次选，不使用 PCMA**；当前主机实际能力为 AMR-NB mode 7，EVS/AMR-WB 尚未接入。本轮不增加语音完成度。

## 生产职责变化

Rust 新增 `usb_sms_receive::Service`，接管 USB AT 短信的自动轮询、手动刷新合并、SM/ME 快照解码、长短信与重复记录处理、接管/预览选择、耐久归档和轮询状态。Go 生产 `startSMSPoller`、`pollSMSOnce`、USB 快照解码与接管编排已移除；相关兼容 helper 仅留在测试中。这与已迁入 Rust 的串口 CMTI 接收是两条不同设备路径。

- 单 worker 在后端 ready 后开始，首次延迟 1.2 秒，后续从上一轮结束起计算间隔，默认 8 秒。手动刷新等待者最多 32 个；采集中收到的新刷新合并为下一轮，不并发执行硬件事务。某个 HTTP 等待取消不会中断已经接受的采集或归档。
- Go 暂留认证私有 `POST /api/internal/usb-sms/snapshot`：固定 USB 句柄，依次执行 CMGF、SM 的 CPMS/CMGL、ME 的 CPMS/CMGL，整组持有 SIM 操作、语音切换与句柄生命周期锁；返回原始响应、分存储错误、采集时间和句柄 session ID。硬件锁在写 HTTP 响应前释放；没有重新加入 Go 解码、缓存或自动删除短信。
- 原始响应每个存储最多 240 KiB，必须收到完整 OK；总请求期限 60 秒。Rust 内部客户端最多接收 1 MiB。部分存储失败仍可保留成功部分；不把超时截断响应当作完整采集。旧会话的传输故障只可关闭其原句柄，不能误关刚重开的设备。新的有界命令还在剩余期限不足1毫秒时停止读取/排空，避免整数毫秒截断为0后变成libusb无期限等待。
- SM/ME 分别解码，避免跨存储拼接长短信；不完整分片不提前发布，冲突分片保持原有处理。跨存储重复按发送方、内容和时间判断，SM 优先，时间降序稳定排序。损坏 PDU 保留既有诊断短信及采集时间回退，不静默丢弃，也不为它伪造正常设备/存储字段。
- 接管开启时先等待 Rust archive 耐久确认，再发布预览；磁盘或确认失败不会假报成功。本轮没有加入自动 USB CMGD。`sms-adopt.json` 配置与实际 USB/libusb owner 仍在 Go；配置读取已经加锁并随每次快照冻结。

Rust `InboxState` 拆为 Go 与 USB 两个分区，公共读取合并后稳定去重、排序并限制为 500 条，持久归档仍优先。旧 Go 展示快照只能更新 Go 分区，不能覆盖 Rust USB 预览。Go 的短信列表、状态、删除及清理兼容入口通过私有 inbox/refresh/prune 适配；元数据缺失、请求失败或确认失败明确返回错误，不再冒充零条短信或删除成功。

## 并发、清理与退出

删除/清理只按开始时快照的记录 ID 修剪 USB 预览，保留期间到达的新 ID。Rust 的预览版本同时阻止清理前已开始的采集重新发布旧记录；未进入归档的旧轮次丢弃，已进入耐久写入的历史事实允许完成，但不能重建旧预览或推进轮询状态。后续新一轮仍可按当前卡片内容重新采集，不将本地预览清理解释为永久屏蔽卡片记录。

Go 手动 USB 删除、按发送方删除及 SM/ME 清理，将 CPMS→CMGD→复核放入同一 SIM 操作锁，避免自动采集在中间把存储切至 ME。该锁内不执行 Rust RPC、归档或预览修剪。仅清理卡片 SM/ME 时保留原有本地缓存语义；选择本地清理时才按选中的快照同步清理对应分区和归档。

每个 Go 进程由 supervisor 注入 producer ID；确认进程退出后，Rust 撤销旧 ID、增加采集 epoch、清除 USB 预览并拒绝旧等待者。原始响应另有固定句柄 session ID，它只代表句柄代次，**不是已验证的 SIM/Profile 身份**。不据此添加 subscription ID 或宣称完成卡片身份租约。

退出先关闭刷新准入，等待 Rust worker 已开始的 HTTP 与归档工作实际结束，再终止 Go；Go 原始采集桥也停止新准入并等待实际事务。不能用中止等待者代替驱动退出确认。真实驱动若不响应期限，仍须报告未确认退出或使用既有进程终止机制；本轮不承诺所有硬件退出都是优雅关闭。

一次手动刷新可能需要等当前 60 秒采集和下一轮 60 秒采集，因此单独使用 Go refresh 客户端 130 秒、Go 写响应 135 秒、Rust 网关 180 秒、Swift 190 秒。共享内部客户端仍为 8 秒，其他普通 API 的期限未扩大。相关请求取消后仍不重放硬件事务。

## 验证与交叉审阅

- Rust daemon 全量 **475 项通过，0 失败、0 忽略**。包括 USB 服务 14 项、归档/预览分区 16 项及内部认证、关闭准入、路由归属与超时测试。USB 回归覆盖单 worker、刷新合并、部分失败、损坏 PDU、归档失败、producer 更换、取消/真实等待，以及在途归档期间清理或换代。
- Go macOS 后端全包 race：**455 个顶层、782 个含子用例通过，0 失败**；2 个既有可选 fixture 因环境未配置而跳过（N待 process fixture、SMS submit parity），不算作通过。最后收紧刷新期限、旧句柄清理及libusb毫秒边界后，定向 race **12 个顶层、28 个含子用例通过，无跳过**。
- Swift 请求策略和真实 Unix socket 超时 fixture 通过；短信 store 的 **17 组生产异步事件测试通过**。这不是完整 App 构建或实机短信验收。
- 按项目要求执行完整 `scripts/test-migration-gates.sh`，exit 0：Rust workspace **958 项通过、0 失败、11 项既有跨语言专用 fixture 忽略**；Go vet/全部兼容包通过，运营商模板 2722 案例、SDP 和 AMR framing 差分通过。脚本中的历史 PCMA helper 差分也通过，它不在生产协商/媒体入口；不把这项验证称为重新启用 PCMA。无 Clippy 通过声明。
- Rust 服务、Go 原始桥、Go 公共兼容入口分别实施并交叉审阅；修正旧快照复活、SM/ME 删除被自动扫描插队、旧会话故障关闭新句柄、等待期限不一致等边界。最终只读复核没有新增阻断项。

主要日志位于 `Switch/build/optimization/2026-10-05-usb-sms-*`：`daemon-tests.log`、`go-cmd-race.jsonl`、`go-raw-final-race.jsonl`、`swift-policy.log`、`swift-store.log`及`migration-gates.log`。验证码提取使用锁定的 regex 1.12.2，以匹配 Go 原有正则语义；仅新增 regex 及其三项依赖，之后的 Cargo 验证使用 offline/locked。未新增真实短信日志输出。

接口清单最终写入与一致性检查通过：**204条method/path，Rust103、Go101，127个Swift调用点，131项指纹审阅范围**。新增13项scope，3项Rust私有动态路由和1项Go原始快照路由均逐项登记；16项契约工具自测通过。Rust-only退出门禁按预期拒绝默认Go fallback及129条Go所有权/依赖路由，不能将这项预期拒绝当作全Rust验收通过。Rust workspace与独立include测试格式、`git diff --check`及本轮文档121个本地链接检查通过。契约日志为`2026-10-05-usb-sms-contract-{write,check,selftests,rust-only}.log`。已有Go manager和imscore/call_runtime的raw string scope词法限制继续由人工审查/race补充，不声称静态门禁覆盖任意行为。

## 进度与剩余范围

已实现并通过离线回归的 USB 自动接收编排计入 P2B：**35.5% → 37.0%**，增加 0.120 计划人周。总计 **11.025 / 47.5 ≈ 23.2%**，估计误差约 ±3 个百分点；P2A 保持 61%，P5 不计分。该分母为原全 Rust 迁移计划，新增 EVS 平台/codec 工作仍需试点后重估，不能把 23.2% 当作新增全部范围的精确完成率。

USB 实体句柄、libusb I/O、设备发现与冲突锁仍由 Go 持有；读卡器/IMS/N待收发及部分配置与兼容入口也尚未退出 Go。完整 USB owner 迁移、插拔/设备阻塞、实际卡片与运营商差分、长稳和异常强杀验收仍缺。没有把原始桥路由标为 Rust-only，也没有将本轮模拟回归当作实机通过。9eSIM 6982 继续排除。

下一轮优先完整迁移 USB AT 传输与事务所有权：Rust 独占句柄、排队、跨 RPC 事务租约、语音重枚举互斥和真实退出等待，覆盖全部现有调用后让 Rust 收件直接使用本地快照。只替换 libusb API 会破坏 Go 锁与 Rust 收件之间的互斥，不能作为验收。已完成只读接缝评估，详见[后续职责审查](optimization/all-rust/modem-next-cutover-review.md)；该建议未计入本轮完成度。
