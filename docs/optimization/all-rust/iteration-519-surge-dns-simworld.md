# Iteration 519: Surge DNS 所有权与 SIM World 对照

2026-10-18。在 `/Users/StatTrak/Desktop/Switch_MAC/src` 修改 Rust 传输选择，不改已安装的 Switch、Surge 规则、号卡 Profile 或实体卡内容；没有发送短信、拨号或生成 DMG。两次实卡会话均使用独立数据目录，测试后停用 VoWiFi 并正常退出。

前置代理的 SOCKS5 UDP ASSOCIATE 不可用时，旧透明路径使用 macOS 主机解析 ePDG，然后检查解析地址是否由 `utun` 接管。这会使 Surge 的域名规则不一定命中，并使 DNS 决策脱离已配置的 Surge 解析器。新的 Surge 透明回退使用 `surge-cli --raw dns lookup` 的有界 JSON 结果；仅在明确观察到没有系统前置代理时才允许主机 DNS 和真实直连。显式或系统前置代理既没有可用 SOCKS5 UDP，又没有经验证的 `utun` 路径时明确拒绝，不静默转为直连；系统代理发现超时、失败或只发现 HTTP/PAC 时也先拒绝。Surge DNS 查询失败不会再使用主机 DNS 兜底。

独立调试观察：旧路径连接的 ePDG 地址属于 `124.217.186.0/24`，Surge 活动请求按 IP-CIDR 规则选择香港家宽节点；CFG_REPLY 提供两个 IPv4 P-CSCF 候选。隧道成功、两个 IMS TCP 候选均超时。改用 Surge DNS 后选择了 `203.145.92.0/24` 的另一 ePDG 地址及另一香港家宽节点，IKE/EAP 和隧道同样成功；IMS 仍在 `plain_connect` 超时，故障计数仍为 `esp_sent=8, esp_received=0, esp_rejected=0`。这排除了单一旧 ePDG 地址或单一节点选择就能解释故障的假设，但不证明代理、ePDG、ESP 封装或内层网络哪一环有错。Surge 官方文档确认 AnyTLS 支持 UDP 中继（UDP over TCP），但某一实际节点及 ePDG 的端到端互通仍须实测：[Surge AnyTLS](https://manual.nssurge.com/policies/anytls.html)、[UDP Relay](https://manual.nssurge.com/policies/udp.html)。

新增解析边界和禁止代理失效后直连的测试。最终完整 Rust 工作区回归为 3031 通过、0 失败、1 忽略；本机实际 Surge 选择专项 1/1 通过。`proletariusd` 构建、Rust 格式、差异检查和迁移契约检查通过，HTTP owner 仍为 294/294 Rust。测试记录位于 `build/reports/iteration-519-rust-workspace-final.log`、`build/reports/iteration-519-real-surge-selection.log` 和 `build/reports/iteration-519-rust-build-final.log`。下一步优先取得非秘密的 ESP 出站与回程证据，并独立核对真实 IKE CHILD SA/ESP 数据面；继续把 IMS 注册、短信和通话列为未验收。仅启用 Surge Enhanced Mode 而未开启系统代理的组合仍需单独核验 DNS 所有权。
