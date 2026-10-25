# Iteration 528：AES-256-CBC ESP 经 SOCKS5 UDP 4500 往返验证

2026-10-18。延续 [Iteration 523](iteration-523-simworld-negotiated-child-sa.md) 的实卡结果：SIM World 协商 AES-256-CBC/HMAC-SHA256-128 后，Rust 本地提交 8 个 ESP，仍未收到入站 ESP。本轮只增加受控本地网络测试；未连接读卡器、切卡、修改 Surge、发送短信、拨号或生成 DMG。

新增测试在本机启动一个受控 SOCKS5 UDP ASSOCIATE relay，以与实卡一致的 AES-256-CBC 和 SHA-256 完整性键长度建立双向 ESP。客户端通过实际 `EpdgDatagramTransport` 向域名形式 ePDG:4500 发送封装的 IPv4 数据，服务端检查 SOCKS5 目标、SPI 和解密后的内层数据；再把反向 SPI 的 CBC ESP 包经同一 relay 送回，客户端实际 `receive_packet` 将其分类为 ESP，并通过 `ChildDataPlane::open_esp` 校验、解密和执行流量选择器策略。专项测试通过，覆盖了原先独立密码向量与 GCM relay 测试之间的组合缺口。

完整迁移门禁通过：Rust workspace **3045 通过、0 失败、1 忽略**，Swift BLE 操作证明及 39 组虚拟事件序列通过，冻结预期完整性与 297 次协议回放通过；Rust-only 源码/构建检查通过。专项日志在 `src/build/reports/iteration-528-cbc-socks-test.log`，门禁日志在 `build/reports/iteration-528-migration-gates.log`（两者均相对 Switch_MAC 根目录）。`cargo fmt --check` 与 `git diff --check` 也通过。

该测试的密钥、SPI、地址与 ePDG 全是合成数据，只证明本地 CBC/SHA-256 路径能经过 SOCKS5 UDP 4500 收发。此前 SIM World 的实卡会话走 Surge 透明代理路径；本测试不能证明 Surge 出口转发了那 8 个实际 ESP，也不能证明 ePDG 接受了真实 CHILD SA 或 P-CSCF 已响应。因此 IMS 注册、短信和通话仍未验收。
