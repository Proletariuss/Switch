# Iteration 604 — SIM World 指定实卡 IMS 注册与隔离清理

日期：2026-10-21。唯一工作区为 `/Users/StatTrak/Desktop/Switch_MAC/`：源码在 `src/`，构建、运行和验收证据在 `build/`，应用与发行物在 `dist/`。没有改动原始参考目录，也没有生成 DMG。

## 实机范围与结果

使用 `dist/Switch.app` 的 3.46/build 346 Rust daemon，在隔离数据根下运行只读范围明确的 VoWiFi 注册探测。探测先连接 ESTKme-RED，并在读卡确认运营商名与预期 SIM World 一致后才启动 VoWiFi；未切换/下载/删除 Profile，不发送短信，不拨打电话。

读卡器返回 SIM World（MCC/MNC 454/03），读取到10个 Profile。当前 SIM World Profile 保持选中。请求的代理字段为空，Rust 自动发现 macOS 系统代理/Surge 状态并应用既有传输选择策略；代理发现失败时不会静默改走主机 DNS/直连。报告没有记录最终传输模式或具体出口，所以只记录“自动选择策略下注册成功”，不声称实际流量经过某个代理节点。状态在约22秒内依次为：隧道 `connecting`、隧道 `connected`/IMS `registering`、IMS `registered`。最终后端状态同时报告 `sms_ready=true`、`call_ready=true`，这只是协议层 readiness，不是实际短信送达或通话成功证据。

探测清理回执确认 VoWiFi 已停止、读卡器已断开、daemon 正常退出；隔离运行目录随后删除。无 Switch 或 `switchd-rs` 进程残留。机器可读结果：[`iteration-604-simworld-vowifi-probe.json`](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-604-simworld-vowifi-probe.json)，SHA-256 `4ea4842742aca36051205a3ba033cd9abc3f902c285e9a45b624034225512ecc`。

## 判定边界

这次通过确认了 **ESTKme-RED + 当前 SIM World Profile + 当前 macOS/Surge 系统代理状态** 这一组实机条件下的 VoWiFi 隧道与 IMS 注册。它关闭了这组条件的读卡/身份及单次注册缺口，但不代表 P1–P4 已全部验收，也不代表其他读卡器、运营商配置或网络组合通过。

本轮没有验证短信真实入站/出站、电话双向音频、EVS/AMR-WB 协商、N待多 Profile 并发、掉线/续期/重认证恢复或长期在线。此前用户明确暂缓入站短信测试；没有因此扩大短信或拨号范围。总体验收分母仍未冻结，整体百分比不报告；294/294 仅是 API 所有权覆盖。

## 项目路径目标

项目方案、执行任务书、全 Rust 计划、当前进度和实施记录已把 `Switch_MAC` 的 `src/`、`build/`、`dist/` 明确为当前读写地址；旧 `/Users/proletarius/Desktop/Switch/` 只保留为历史地址。Codex 内部 goal 的 objective 仍存旧路径；本会话提供的 goal 工具仅支持读取和更新状态，不支持改写 objective 文本，因此此内部字段无法由当前工具修正。仓库内的可执行目标和所有本轮材料均已使用正确路径。

## P0 静态源映射纠错

同轮核对发现 `runtime-owner-format-exit-map.json` 的 `still_unknown_or_pending` 仍写着“Serial physical CloseACK differs from native actor thread.join; constructor does not retain that thread handle in graph.” 这条表述已被 Iteration 603 的源码变更取代。当前源码中的 `modem_actor.rs` 在 actor 建立时将线程句柄保存在共享 owner（line 896），终态 `Finalize` 关闭 session 并结束 actor（line 2014），`shutdown` 在最终回执后实际调用 `JoinHandle::join()`（line 1606）；回归 `final_shutdown_waits_for_actor_close_and_joins_owner_thread`（line 5073）在完整测试日志中通过。

为避免依赖旧快照推断，本轮逐项比对 Iteration 603 source snapshot 的179个文件，缺失0、内容变化0；`modem_actor.rs` SHA-256 为 `2e57fbd710204d6672320e211fcccb1ed22c7974f78a27c6ec0c7bb98d915d4a`。对应完整 daemon 日志 [`iteration-603-daemon-correct-env.log`](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-603-daemon-correct-env.log)，SHA-256 `5fd26e9d9844c607a8736e86b5fce2658d1cb7c40d9c65bff5b7415e345c674f`：3036通过、0失败、8忽略；named join test 出现在通过日志。映射现移除这条过时 UNKNOWN，并保留其余真实待办。此项是 P0 证据索引纠正，不代表串口实体设备关闭验收，也不新增功能需求或整体完成率。
