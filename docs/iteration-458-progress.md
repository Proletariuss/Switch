# Iteration 458：执行 P2A 转发与 CFNetwork/PAC 离线验收

本轮补齐已有 Rust 短信转发实现的本机离线证据，没有变更转发业务 owner，也不将离线夹具算作真实 provider 验收。

macOS 上 `proletarius-system-proxy` 的 6 项测试全部通过，其中 `cfnetwork_executes_synthetic_pac_without_network_access` 直接调用 CFNetwork 对内存中的合成 PAC 脚本求值，覆盖 PAC 返回代理、DIRECT 和代理候选回退；不更改系统代理、不访问 PAC URL 或外部网络。`proletarius-forwarding` 通过 56 项测试，2 项 Go flock peer 测试按设计忽略，随后由跨进程脚本分别验证 Rust→Go 与 Go→Rust 的 SWF1 文件锁互斥。SWF1 状态兼容脚本通过，包含 Go 生成 golden 解密/重加密相等及 Rust/Go 状态迁移校验。

证据日志位于 `build/runtime/iteration458-forwarding-lock-interop.log` 与 `build/runtime/iteration458-forwarding-state-compat.log`。这些结果证明合成 PAC 求值和离线状态兼容，不证明当前 macOS/Surge 系统代理设置、PAC URL 下载、Telegram/Bark 外网送达或物理短信到达；真实 provider/设备验收仍未完成。

本轮不改变全 Rust 加权迁移值 **37.99%（18.045/47.5）**，P2A 业务 owner 已在 Rust；剩余主要为系统配置/真实 provider/硬件验收证据及其它 P2B–P6 Go owner。没有启动 app、读卡器、SIM、发送短信或运营商网络，也未生成 DMG。
