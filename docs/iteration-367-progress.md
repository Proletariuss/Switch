# Iteration 367：IMS AKA nonce 解码迁入 Rust

日期：2026-10-11

读卡器 VoWiFi 的主 IMS REGISTER 与附属 `voiceclient` REGISTER 生产路径现在把 Digest challenge 中的 AKA nonce 交给同一个 Rust IMS codec endpoint。Rust 兼容解码 hex、带/不带 padding 的 standard 与 URL-safe base64，提取固定 16-byte RAND 和 16-byte AUTN，并忽略仅用于服务器侧的后缀数据；原始 nonce 仍保留给 Digest 计算。Rust endpoint 按 IMS challenge 头部上限约束输入并清除临时 nonce 字符串。Go bridge 只校验 Rust 返回的固定长度数组，再交给既有物理 SIM AKA 适配器。

生产 reader session 配置 Rust decoder 后，IPC 缺失、坏响应或 nonce 解码错误会在执行任何 SIM AKA 命令前失败关闭，不回退到 Go 解码。没有配置 decoder 的旧兼容调用仍保留原行为。RAND/AUTN 的物理 AKA APDU、RES/AUTS 处理、SIP stream/socket、事务等待和 IMS 生命周期仍由 Go 承担；本轮不代表 IMS/VoWiFi 完整迁移。

定向测试覆盖 Rust nonce 解码 operation、支持的编码与服务端后缀、Go Rust-bridge 数组校验、两条 IMS 注册路径的生产 callback 注入，以及 decoder 错误时 AKA 不执行。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,908 passed / 6 ignored**（daemon **1,371 passed / 3 ignored**），Go 全包 test/vet、Swift BLE 回归及协议 parity 通过；契约清单 **343 method/path（Rust 283 / Go 60）**、127 Swift 调用点、353 review scopes，契约 `--diff`、`cargo fmt --check` 与 `git diff --check` 通过。日志：[iteration-367-migration-gates.log](../build/optimization/iteration-367-migration-gates.log)。

P3 增计 **0.025 计划人周等值**，累计 **1.915 / 8.0**；项目累计 **16.395 / 47.5 = 34.5158%**，显示 **34.5%**（两位小数 **34.52%**，估值误差约 ±3 个百分点）。

没有连接实体 SIM/读卡器，没有运营商、短信或通话实测，也未生成 DMG。
