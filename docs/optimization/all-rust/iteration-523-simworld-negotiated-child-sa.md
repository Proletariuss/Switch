# Iteration 523：SIM World 实卡 CHILD SA 协商核对

2026-10-18。用 ESTKme-RED 中**当前已启用**的 SIM World 454/03，在独立 Rust 后端及隔离数据目录复测。只连接和读取读卡器、启动 VoWiFi；没有切卡、下载 eSIM、发送短信、拨号、改变 Surge 规则、替换已安装 App 或生成 DMG。测试后明确停止 VoWiFi、断开隔离读卡器会话，后端正常退出且清理已确认。

临时、仅用于诊断的构建在 IKE_AUTH 完成后记录协商参数：ESP 选中 transform `12`（AES-256-CBC）、完整性 transform `12`（HMAC-SHA256-128），加密键和完整性键长度各 32 字节。TSi 为单 IPv4 主机，TSr 为 IPv4 范围，双方协议任意、端口 0–65535。临时代码已撤回；脱敏结果见 `build/reports/iteration-523-simworld-child-sa.json`。实际 SPI 值、内部地址、卡号和密钥均不保留在交付记录中。

隧道曾达到 `connected`，随后 IMS `plain_connect` 超时，最终诊断仍是 `esp_sent=8、esp_received=0、esp_rejected=0`。这与 [Iteration 522](iteration-522-esp-independent-vectors.md) 的独立 AES-256-CBC/HMAC-SHA256-128 线格式向量相对应，排除了“仅核对了另一种 ESP 套件”的证据缺口；仍不能证明这次会话的实际 ESP 已从代理出口到达 ePDG，或内层 P-CSCF 可以回应。该测试没有完成 IMS 注册，也没有短信或通话验收。

首次隔离启动误用了旧构建目录的后端，因此没有产生新增协商字段；关闭该会话后，使用经二进制字符串核对包含诊断标记的**新构建**在第二个隔离目录复测，以上结果均来自第二次会话。原始进程日志和隔离运行数据已在脱敏后清理；临时代码撤回后，日常后端重新构建通过。下一步应取得代理出口或 ePDG 侧 UDP 4500/ESP 接收证据，或以独立已知正常客户端在同一网络、同一卡上进行单变量对照；在此之前不改运营商模板、DNS 或代理路径。
