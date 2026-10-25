# Iteration 521: SIM World NAT-T 流量边界对照

2026-10-18。使用 ESTKme-RED 中当前已启用的 SIM World profile，在两个独立 Rust 数据目录复测。没有切卡、下载 eSIM、发短信、拨号、修改 Surge 规则、替换已安装 App 或生成 DMG。两次隔离会话均已停用并正常退出。

原实现的 UDP socket 在 IKE_SA_INIT 期间以本地 500 端口连接 ePDG:500，之后保持本地端口并改连 ePDG:4500。本机 `lsof` 在 IMS 阶段确认实际目的端口为 4500；Surge 的同一连接记录也出现先后指向远端 500 和 4500 的条目。IMS 发出 ESP 期间，Surge 出站字节增加而入站字节保持不变，最终 `plain_connect` 超时，`esp_sent=8、esp_received=0`。因此“Surge 从未看见 4500 目标”不符合本次观测。

为排查 Network Extension 按原始五元组固定 UDP 流的可能性，临时将 500→4500 切换改为新建独立 UDP socket。专项单测和构建通过；实卡上 Surge 确认出现独立的 4500 UDP 流，且该流收到 IKE 回复。但 IMS 期间它同样只有出站字节增加，没有收到新的 ESP，最后仍为 `esp_sent=8、esp_received=0` 与 `plain_connect` 超时。此次自动策略分别选中香港 14、15 家宽节点，出口并不相同，不能据此做严格的单变量性能比较；但至少确认独立 4500 流仍不能使该卡 IMS 注册。实验代码及其测试已撤回，Rust 开发后端按原源码重建通过。

[RFC 7296 §2.11、§2.23](https://www.rfc-editor.org/rfc/rfc7296.html)规定 IKE 在 NAT 情况下应支持端口变化，并定义 4500 的 IKE/ESP UDP 封装。本次本机与 Surge 计数只能证明客户端提交了报文、Surge 接管了对应流，并收到过 IKE 回复；不能证明代理出口将全部 ESP 交付 ePDG，或 ePDG 是否因 ESP/内层目标/策略拒绝而不回包。`Surge failed=false` 也不等于运营商接收成功。脱敏的逐流元数据保存在 `build/reports/iteration-521-simworld-flow-evidence.json`，不含卡号、身份、密钥、报文内容或精确目标 IP。

下一步先取得代理出口或 ePDG 侧的 UDP 4500 接收证据；若不可得，则以独立的已知正常 ESP 实现或标准向量核对实际协商的 CHILD SA 套件、密钥方向、SPI、ESP 帧与流量选择器。没有新证据前不修改 SIM World 模板，也不宣称 IMS、短信或通话验收。HTTP 契约归属仍按 Iteration 520 的 294/294 Rust 门禁记录，本轮没有增加迁移比例。
