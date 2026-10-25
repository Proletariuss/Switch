# Iteration 492：完整语音与续期恢复组合、原生状态等价修复

2026-10-14。延续全 Rust 后端目标，用户本轮明确要求生成 DMG；已交付 3.31 / build 331，包含 Iteration 491 和本轮新增源码。最终发行证据在 `../build/reports/release-3.31/verification.json`。Release 构建、深度严格签名、DMG 校验、实际只读挂载、包内与构建应用逐文件一致性、4个Mach-O架构及加载依赖、13份许可证内容均通过独立核验。本机ad-hoc签名，未公证、未实机。

## 生产修复

- 原生发送短信就绪同时要求已注册、consumer 已启动、SMSC 与目标非空白。心跳恢复仍检查路由条件。缺少发送路由不会关闭入站短信和语音就绪；实际发送入口同样拒绝空白路由。
- 分别投影隧道、IMS 注册和业务就绪，补齐进度描述，隧道建立后即可展示实际地址。仅采纳同代且仍处于启动或注册阶段的事实；旧代、退避、排空及停止状态不能重新获得就绪。
- 通话历史观察任务启动失败时保留已注册通信，并在历史接口显式显示错误。未创建 worker 的本代不占用归档源，不清空旧 worker 的生产者、冻结记录、待写数据及错误；已有 worker 仍须真实退出后才释放对应责任。归档失败不会伪称成功。

## 完整本地组合证明

### 双向呼叫和媒体

真实 Service / LiveDriver、模拟物理卡租约、SOCKS、IKE、双层 ESP 与 REGISTER 后，经公开接口发起 EVS 呼出及接听 AMR-WB 呼入。实际 codec 编解码，每通四帧非静音 RTP/PCM 回送，核对序号、时间戳、编码模式和解码错误；主机 PCM token 错误被拒绝。检查 DTMF `#` 七个事件包与结束位、本地和远端挂断、两个已接通历史记录、媒体端口重新绑定、租约释放及认证 IKE DELETE。

覆盖 EVS primary mode 4 compact 和 AMR-WB mode 8 octet-aligned；不启用 AMR-NB 或 PCMA。模拟媒体对端回送实际编码数据，不代表独立运营商编码互通、真实声卡听感、长通话或丢包恢复。

### 三代续期与恢复

运营商模拟端授予 90 秒 REGISTER 租期，生产计时规则在真实经过 30 秒后触发续期。第 27 秒入站短信已耐久归档，持有 RP-ACK 的最终响应跨越续期点。断言同一真实 ticket 得到 `Accepted`，且 `renewal_reports_drained=true`，续期点仍有一项待回执；然后确认 consumer 实际退出、物理租约释放与认证 IKE DELETE，再获取同卡第二代。

第二代实际受保护 TCP 关闭，joined actor 必须返回 `StreamClosed`，完成清理后第三代恢复；第三代经公开发送入口完成 RP-DATA / RP-ACK，第一、二代旧能力被拒绝。共三次注册、三次 IKE DELETE 和六次模拟物理 AKA。未注入 due 事件、未推进虚拟时钟，也不将超时作为成功。此组合不覆盖 WAN/IKE 黑洞或活跃通话中续期。

交叉审阅曾指出只检查 pending 队列清空无法区分 Accepted 与超时；已加强到真实 ticket 终态。末项完成通知可能留在真实 joined consumer 的 `undelivered_notice`，检查合并已发布和该未投递通知，不改变生产处理。首轮观察位置失败日志保留，修正后专项通过。

### 降级链路

预先保留旧历史任务，使本代无法创建归档任务，实际 REGISTER 和入站短信耐久落库、SIP/RP 确认仍完成。有发送路由时实际发送成功，无路由时拒绝发送能力而保留入站和语音就绪。退出保留归档错误，旧历史任务仍存活；不冒充旧任务已退出。此降级组合验证语音就绪，实际双向通话由正常路由的独立语音组合证明。

## 验证

| 验证 | 结果 |
| --- | --- |
| Rust workspace，locked，4测试线程 | 2636 passed / 0 failed / 6 ignored；daemon 1906 / 0 / 3 |
| Go | vet及全包测试通过 |
| Swift BLE | operation proof及manager 38组通过 |
| 跨语言差分 | 12组通过，包含2726项carrier replay |
| 契约与格式 | Rustfmt通过；45项扫描器自测通过；342契约（Rust305/Go37）、127 Swift调用点、434审阅范围、51 HEAD别名 |

日志使用 `../build/reports/iteration-492-*` 与 `../build/reports/release-3.31/`。三路独立交叉审阅已完成；变更前基线为 `iteration-492-baseline`，4681 个文件。

## 完成边界

未操作真实读卡器、未发送外网短信、未拨打真实电话；保持既有原生默认 gate 关闭。Go 退出仍有 6 类阻塞、257 个生产文件。历史加权估值 38.31% 保留为历史核算值，不按测试或迭代数增加进度。后续仍需完成 N待、原生默认接管、Go 退出、其他完整恢复场景与实机验收。此次阶段 DMG 不表示全 Rust 目标完成。

## 本轮安装包

[Switch 3.31 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.31-macOS27.dmg)，构建331，Apple Silicon，macOS 27.0及以上。

SHA-256：`c6a7fd33eee65d05a77f697581605ce951213795edfb4bee667f94486fca091c`。
