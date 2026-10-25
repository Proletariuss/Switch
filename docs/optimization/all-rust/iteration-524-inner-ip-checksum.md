# Iteration 524：用户态 TCP SYN 与 ESP 数据面边界核对

2026-10-18。延续 [Iteration 523](iteration-523-simworld-negotiated-child-sa.md) 中 SIM World 已协商 AES-256-CBC/HMAC-SHA256-128、但 `esp_sent=8、esp_received=0` 的结果。本轮仅改 Rust 回归测试；没有连接读卡器、改变代理、切卡、发短信、拨号或生成 DMG。

`TunnelIpDevice` 的实际 smoltcp 出站队列生成一个 IPv4 TCP SYN，测试使用独立的 Internet checksum 求和核对 IPv4 头部、TCP 伪头部和 TCP 段，同时检查总长度、地址、端口和 SYN 标志；单字节篡改须使校验失败。这样可以验证该用户态 IP 设备在无网卡校验和卸载的前提下确实形成可用 SYN。`child_data` 原有 AES-CBC/HMAC-SHA256 方向键与策略测试扩展到 128 和 **256 位** AES，覆盖 SIM World 本次实际协商的密钥长度。

验证：`cargo +1.98.0 test --locked -p proletarius-vowifi` 最终为 281 通过、0 失败；`cargo fmt --check` 与 `git diff --check` 通过。日志见 `build/reports/iteration-524-vowifi-tests-final.log`。本轮没有修改生产数据面，因此无需再用同一卡重复同一网络条件下的连接尝试。

范围限制：测试用的是合成 IPv4 端点，不能证明实卡会话中的每个内层报文、IMS 安全变换或代理出口都正确；ESP 完整线格式已由 [Iteration 522](iteration-522-esp-independent-vectors.md) 的独立向量覆盖，但实际 UDP 4500 包是否抵达 ePDG 仍缺外部接收证据。不能据此宣称 SIM World IMS、短信或通话验收完成。
