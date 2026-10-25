# Iteration 464：DITO 的 SHA1-96 CHILD SA 数据面闭环

在为 DITO 515/66 新增完整 native startup loopback 时发现一处真实断层：carrier proposal、MODP group 2、IKE SK 和 ESP HMAC-SHA1-96 已能协商，但 `PendingChildSa::into_data_plane` 只允许 ESP HMAC-SHA2，仍会以 `UnsupportedAlgorithm` 拒绝 transform 2，因而认证完成后不能安装 CHILD SA。现已允许 AES-CBC 与显式协商的 HMAC-SHA1-96 组合进入现有定向 ESP 收发器；没有放宽 proposal 匹配、跳过 peer 完整性校验或加入算法回退。

扩展本地模拟 ePDG/SOCKS5 UDP loopback：DITO profile 515/66 使用精确 `aes128-sha1-prfsha1-modp1024` IKE 与 `aes128-sha1` ESP；模拟响应端根据实际 KE 的 DH group 生成 group 2/14 密钥。测试验证所选 proposal/KE、SA_INIT、EAP-AKA、IKE_AUTH/CHILD SA、双向加密 ESP SIP 包、DPD、篡改包拒绝、peer IKE DELETE，以及应用启动失败时不留下活动数据面。此为合成离线协议证据，不是实体 SIM/ePDG/IMS 注册、短信、通话或运营商兼容性验收。

同时修复了 Rust lease worker 的退出顺序竞态：在 FIFO permit 释放前先撤掉物理 worker 标记，避免下一项操作刚获得串行锁时误收 `Busy`。原 overflow/队列用例在并行全测中曾复现该问题；修复后定向回归和完整 workspace 均通过。

验证：`cargo fmt --check` 通过；`cargo test --locked -p proletarius-ike` **43 passed**；`cargo test --locked -p proletarius-vowifi` **190 passed**（188 单元/loopback + 2 carrier integration），0 failed。全量 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,466 passed / 0 failed / 3 ignored**，Go 全仓 test/vet、Swift BLE、carrier replay 及 USB/radio/HTTP/IMS/USSD/call parity 通过；队列 overflow 用例在全量并行运行中通过。API 合同 `--diff` 通过：354 method/path contracts（Rust293/Go61）、127 Swift HTTP calls、379 review scopes；Go 退出审计仍为6类/266个生产Go源。`git diff --check` 通过。

测试日志 SHA-256：`ed93beb0652e7e46bf42a6ceb9d2f1e12d5eee3182cee4c0943583f9238e2000`，见[协议测试日志](../build/reports/iteration-464-ike-vowifi-tests.log)；完整门禁 SHA-256：`1e083b17c5122a193ff8242d1667dad34d99e98a83dd8cfc6fd7ba2e3f65b70a`，见[迁移门禁日志](../build/reports/iteration-464-migration-gates.log)；[合同差异日志](../build/reports/iteration-464-contract-diff.log)；[Go退出审计](../build/reports/iteration-464-go-exit-audit.log)。

该修复没有切换生产VoWiFi owner：`reader_vowifi` 仍是 Go 持有的协议影子路径，Go 退出审计仍为6类阻塞/266个生产Go文件，故全 Rust 加权进度保持 **18.045/47.5 = 37.99%**。没有连接实体卡或真实网络；9eSIM `6982` 继续排除。没有新建DMG。
