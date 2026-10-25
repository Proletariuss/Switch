# Iteration 463：DITO Philippines 的显式 IKE group 2 / SHA-1 支持

根据最新同步的 DITO 515/66 carrier profile，在 Rust native IKE/ESP 链路补齐其明确声明的 `AES-128-CBC / HMAC-SHA1-96 / PRF-SHA1 / MODP group 2` 提案。实现涵盖 proposal 编码、MODP-1024 密钥交换、IKE SK 的 HMAC-SHA1-96、ESP CBC 的 HMAC-SHA1-96，以及认证密钥长度与 IKE_SA_INIT KE group 的校验。启动时从实际 IKE proposals 读取 DH group 并生成相应 KE；多个 proposal 若要求不同 DH group 则拒绝启动，避免发出无法匹配的 KE。

group 2 是 RFC 2409 定义的 1024-bit MODP 旧群，强度低于 group 14。实现只在有效 carrier proposal 明确要求时使用；不在更强组失败后回退，也不扩大到其他运营商模板。来源：[RFC 2409 §6.2](https://www.rfc-editor.org/info/rfc2409/)。

回归覆盖 DITO upstream 配置到 native proposals 的精确转换、group 2 公私钥互操作及 peer 值检查、离线完整 SA_INIT group 2/SHA-1 协商、IKE SHA1-96 SK 与 ESP SHA1-96 往返及篡改拒绝。现有 CBC/SHA-2、GCM、group 14 与 startup cancellation 测试保持通过。

验证：完整 `scripts/test-migration-gates.sh` 通过；Rust workspace 汇总 **2,104 passed / 0 failed / 6 ignored**，Go 全仓 test/vet、Swift BLE 38 组虚拟序列、2,726 carrier replay、USB/radio/HTTP/IMS/USSD/call/SMS parity 均通过。测试期间 workspace 并发队列用例曾单独出现一次 `Busy`，单项复跑通过，随后完整门禁通过。`git diff --check` 通过。门禁日志 SHA-256：`a842d08b188a231506d06d14d47d152a96e45690efa165acd6032cb13dc10193`；[门禁日志](../build/reports/iteration-463-migration-gates.log)。

Go 退出审计仍有 **6 类构建/运行/源码阻塞**，包含 Xcode/app 的 Go build 与嵌入、Rust daemon 到 Go socket 的转发、Go module，以及 **266 个生产 Go 源文件**；[退出审计](../build/reports/iteration-463-go-exit-audit.log)，SHA-256 `697347b90d90f0261b3f1ab777e2296c557ff8b0c10a985e869a2442d48819a3`。本轮增加 carrier 协议覆盖，没有迁移新的生产业务 owner，故全 Rust 加权进度仍为 **18.045/47.5 = 37.99%**。

这是离线协议测试，不代表 DITO 网络或 SIM 的实网注册、短信、语音已验证。未操作实体卡或运营商网络。没有重建 DMG；Iteration 462 的 DMG 不包含本轮源码。
