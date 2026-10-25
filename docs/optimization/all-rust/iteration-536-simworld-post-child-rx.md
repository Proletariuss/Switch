# Iteration 536：SIM World CHILD SA 后的入站分类与直连对照

日期：2026-10-18。

## 改动

Rust 隧道数据面新增三个仅含数量的会话计数：CHILD SA 建立后的 IKE 控制包、NAT-T 保活以及传输分类阶段拒绝的无效数据报。它们与现有 ESP 发送、接收和解密拒绝计数一起进入 IMS 启动失败诊断。计数在真实接收分支更新，不保存 SIM 身份、地址、密钥、包内容或单包时间。受控 SOCKS5 UDP 混合流量测试覆盖了保活、短无效数据报、IKE 与 ESP 的分类；静态诊断测试核对新字段不会出现在无关故障中。

## ESTKme-RED + SIM World 实卡结果

两次测试均在隔离 Rust daemon 和数据目录执行，确认 10 个 profile 中当前启用的是 SIM World；只连接、读取并尝试 VoWiFi 注册。没有切卡、下载、发短信或拨号。每次结束都明确停止 VoWiFi、断开读卡器，daemon 零退出。

| 路由 | 网络阶段 | 后端计数 | Surge 记录 |
| --- | --- | --- | --- |
| 原香港家宽代理 | IKE/EAP 成功，IMS `plain_connect` 超时 | ESP 发出 8、收到 0、拒绝 0；CHILD 后 IKE/保活/无效数据报各 0 | 香港 15 家宽；出站 2051、入站 1188 字节。入站总量含 IKE 握手回复 |
| 临时精确直连 | IKE_SA_INIT 超时，未到 CHILD/IMS | 无 ESP 阶段计数 | `DIRECT`；出站 1434、入站 0 字节 |

ePDG DNS 在测试间切换地址，第一次只覆盖旧地址的临时规则未命中；它实际仍走香港 13 家宽，并复现了与首行相同的 ESP 8/0 和 CHILD 后所有入站分类为 0。该次记录为第二次**代理**测试，不计入直连对照。随后针对两次观察到的精确地址同时下发临时规则，Surge 对新进程实际记录为 `DIRECT`，才形成表中的直连结果；规则已全部移除，当前临时规则列表为空。本机 Surge SOCKS5 监听对 UDP ASSOCIATE 返回不支持，无法在同一节点上使用显式 SOCKS5 UDP 做第三种对照。

这证明在当前网络中直连连 IKE 握手都无法完成，而香港家宽代理可以建立 IKE/EAP。代理路径在 CHILD 后本机未收到任何 UDP 数据报；本地计数和 Surge 本机连接字节仍不能证明 ESP 已到达代理出口/ePDG，也不能判定 ePDG 为何没有返回数据。SIM World IMS 注册、短信、通话、长稳均未验收。下一步需要代理出口或 ePDG 侧 UDP 4500 证据，或同卡同网已知正常客户端的严格对照，不能再凭本机计数盲改模板或 DNS。

脱敏证据：`build/reports/iteration-536-simworld-proxy-sanitized.json`、`iteration-536-simworld-proxy-second-sanitized.json`、`iteration-536-simworld-direct-sanitized.json`。Rust workspace 回归 3071 通过、0 失败、1 忽略；Go-free 结构门禁通过，API 清单仍为 294/294 Rust 归属。未生成 DMG。
