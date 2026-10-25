# Iteration 647：QMI open/close 的 P0 有限静态责任图

本轮只审阅 `POST /api/internal/qmi/open`、`/close` 与自动 QMI 启动/热插拔重试共享的会话责任链。对应现有需求叶子是 `P0-2.owner-map` 与 `P2B-4.qmi-control`；两者在 [验收台账](acceptance-ledger-2026-10-19.json)仍为 `NOT_REVIEWED`、正式适用实例数组仍为空。`P2B-4.qmi-uim` 叶子在本轮范围外，不能从本记录推断其 UIM 业务验收。 [Iteration 645 runtime 映射](runtime-owner-format-exit-map-iteration-645.json)的 `device::optional_QMI_start_and_session_finalizer` 是未计分历史图，仍描述一次性 `NativeQmiLifecycle`；当前源码改为保留的 `NativeQmiRetryLifecycle`。本记录为这一入口群补充当前源码的有限静态覆盖，不修改历史原文、正式实例登记、需求状态或总分母。

| 责任 | 已核对的当前路径 | 仍需区分的结果 |
| --- | --- | --- |
| Producer / 准入 | `main.rs` 有两条通向同一 handler 的 QMI 路由：私有 Unix socket 在 `handle_internal_request` 的 1695 行识别 QMI 前缀、1729–1734 行鉴权、1853–1854 行分派；HTTP gateway 在 `handle_request_inner` 的 1993–1995 行纳入 QMI 前缀、2033–2038 行鉴权、2090–2092 行分派。handler 仅接纳 POST，把最多 16 KiB JSON 解为带 `control_path` 的 open 或空对象 close。另一路 producer 是由 main 保留句柄的 QMI retry worker。 | 各路鉴权各自固定，不把一条路的鉴权误当另一条的证明。两路共享同一 `QmiEnhancement`，并非两份独立物理 session。私有 Unix socket 停机只放行 close，不放行新 open。 |
| Owner / 语义选择 | `QmiEnhancement` 的 `Arc<Mutex<QmiState>>` 保存单一 session 槽、控制路径、private-close 标志、manual epoch 和 cleanup failure。private open 只有在已选择相同路径且无未确认清理时才能授权重试；private close 先设 fence、推进 epoch、清空重试 selector，再执行 close。 | 自动选择与精确 VID:PID 选择不可混用；已关闭状态不能由后台 retry 隐式撤销。配置和运行结果不等于实体模块已就绪。 |
| Format / Writer | QMI core 以 service/client/transaction/message/TLV 编码请求，在 write mutex 下交给 transport；macOS libusb 单次 claim 候选接口并以 CTL_SYNC 确认后才发布 client，bulk writer 使用所选 endpoint 和有界超时。IMS/NAS/UIM 建立失败走带清理结果的路径。 | 静态代码可以定位真正 USB writer；没有实体 DJI 模块的 CTL_SYNC、响应、吞吐或网络业务收据。 |
| Exit / 失败保留 | session close 汇总 UIM/NAS/IMS/USB client 的结果；core client 关闭 transport 并 join reader；libusb release 分类 `NoDevice` 与其他未确认错误。retry loop 只有旧生命周期 shutdown 已确认才进入下一次重试；retry shutdown 停止并 join worker，main 等待 QMI 关闭结果进入 shutdown report。迟到启动交接失败时原会话仍需 close。 | `cleanup_failure`、未知 close 与 panic 不能当成功，也不能允许无条件重新 open；静态路径和软件故障注入不代替物理拔插/重枚举验收。 |

[机器可读审议记录](iteration-647-p0-qmi-owner-trace.json)把上述 27 个源码范围逐一固定到当前文件哈希，并登记 8 个**候选软件分支**：AT 模式无 retry、自动选择缺设备、CTL_SYNC 成功、精确 selector 重连、private close fence、迟到 open/取消、断线且清理不确定、daemon shutdown join。每个分支只描述触发条件、预期静态路径和对应锚点，均明确 `hardware_verified=false`、`acceptance_pass_awarded=false`；它们不是设备/固件/卡/运营商的正式支持实例，也不构成冻结分母。i646 源快照保持原样；新增记录另行锁定 QMI core 与 macOS libusb 源文件。

现有 [i646 全量迁移门禁日志](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-646-migration-gates.log)包括 `native_qmi_retry_private_close_stays_closed_until_private_open`、`native_qmi_retry_private_close_during_open_fences_stale_handoff`、`native_qmi_retry_keeps_private_exact_selector_after_reader_loss`、`native_qmi_retry_stop_during_late_open_closes_once_and_never_reopens` 和 `unknown_probe_cleanup_is_typed_and_stops_candidate_scan` 通过。这些是软件回归。新增验收台账静态校验逐一检查 27 个锚点、6 个文件哈希、两条路由的鉴权/分派配对、i645 历史图与 i646 快照身份、8 个候选分支的固定锚点与语义声明，以及未授予 PASS/未冻结分母标志；负向测试会实际拒绝无关锚点、占位语义、缺失退出锚点、源/行漂移和误标 PASS。

余项：旧 macOS QMI/MBIM 可达型号、固件及操作支持集合未从历史材料完整恢复；当前 USB 树未枚举 DJI 一代，真实 CTL_SYNC、拔插、release 和 service 响应未验；`probe`、`set-ims`、`operator` 以及其他 P0 producer/writer/exit 家族未纳入这个有限切片。P0 仍未闭合，P1–P4 实体/运营商验收、P5 实际双向通话与长稳、P6 最终发行和外部 CI 等仍待完成；总分母未冻结，不报告整体百分比。本轮不访问硬件、不改生产源码、不生成 DMG。
