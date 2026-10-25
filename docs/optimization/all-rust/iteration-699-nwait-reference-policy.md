# Iteration 699：参考 2.67 补齐 Rust N待保活策略

本批按用户新提供的 `/Users/StatTrak/Downloads/Switch-2.67-source/` 只读比较 N待。审阅绑定 15 个旧文件和 37 个当前文件，未执行、导入旧 Go 服务，也未读取旧运行数据。当前业务继续由 Rust owner 执行。

## 已实施与验证

- 旧版对 `SMSOnly && apple_vodafone_uk` 明确关闭主动 OPTIONS；现已补入 Rust 的内部会话模式和冻结策略。普通通话会话及其他运营商保持主动探测，配置缺失仍拒绝，不把全局零间隔当作禁探测授权。
- 保留 TCP keepalive、入站 OPTIONS 响应、真实短信确认、授予租期、原关联 REGISTER refresh、EOF、停止与实际网络清理；未更改代理、ePDG/端口优先级或语音编码策略。
- VoWiFi 库全量 **298 通过、0 失败**。该次执行绑定 v1 清单，后续只有 daemon 测试文件变动，库及依赖输入未变；不把旧清单冒充后续整树的运行证据。
- daemon 三项真实本地 SOCKS/IKE/ESP/SIP 组合测试 **3 通过、0 失败**：Vodafone N待无主动 OPTIONS、其他运营商仍主动探测、普通 Vodafone 会话仍主动探测。覆盖实际 N待归档先于 SIP 确认、RP 确认、同关联 401/AKA/200 续期、跨旧租期、EOF/主动停止和实际 join。
- 普通 Rust 后端 `check` 通过，当前主机依赖图没有启用 fault/authority-codec 测试特性。这不是 Release/App 编译证明。
- v3 冻结 715 项 Rust 输入；3446 项 daemon 测试登记保留此前全部 3443 名称，仅增加上述三项。登记不代表全部执行。

## 失败与修正保留

首轮组合为 1 通过、2 失败。新测试报文漏了 `P-Asserted-Identity`：真实生产路径已归档，但无法取得 IP-SM-GW 回执目标。仅补正测试同域身份头，保留归档、网络 ACK、续期及清理断言，生产逻辑不放宽；原失败日志、源码原像、补正差异及复验全部保存。

测试子进程另补 60 秒观测期限与 stdout/stderr 各 1 MiB 上限；成功必须真实进程退出及双输出 EOF，错误走该子进程的终止/回收。期限不等于 OS 回收完成证明；强制销毁运行时、内核失败和任意外部预置环境不在成功声明内。本轮 runner 清除继承的 child marker，使用工作区根和 umask 077。

首次无平台过滤的离线 metadata 因其他目标图缺 `core_detect` 失败，未下载依赖；改为当前 `aarch64-apple-darwin` 图后通过。两次日志均保留。

## 下一步与取舍

现有 Rust 已有 startup gate、短物理 FIFO、身份读回、typed 重连、原卡恢复、持久化请求及未知发送围栏；不恢复旧版无界队列、所有错误一律重连或整关联到期重建。

下一批优先补不同订户身份的真实多线路 A/B/A 续期：旧 fixture 两个 ICCID 共用 IMSI/AKA，不能充分检测鉴权串线。随后补停止、恢复原卡与断链交错。为避免相邻测试夹具补齐各重复约半小时的完整 daemon 执行，将全量回归合并到该 N待组合批冻结后进行；**本批尚无当前源码完整 daemon PASS**，698 的全量结果不继承。

整体验收仍在 P6 稳定性收口，P0–P6 未完成；正式台账不因本批局部通过重计，整体分母尚未冻结。真实读卡器、多 profile 运营商互通、24/72 小时、完整冷 App 与最终三轮审议仍开放。本批没有设备/运营商动作、短信/拨号、外部推送或 DMG。

证据位于 `/Users/StatTrak/Desktop/Switch_MAC/build/reports/stability-699/`，入口为 `execution-summary.json`；旧版比较见 `nwait-267-reference-audio.md`，后续组合计划见 `nwait-next-acceptance-audit-socket.json`。
