# Iteration 366：IMS REGISTER Digest challenge 解析迁入 Rust

日期：2026-10-11

读卡器 VoWiFi 的主 IMS REGISTER 注册路径和附属 IMS client 现在将原始 `WWW-Authenticate` / `Proxy-Authenticate` Digest challenge 交给 Rust IMS codec 解析。Rust 返回有界、类型化的 realm、domain、nonce、opaque、stale、algorithm、qop、charset 和 userhash 字段；Go 再使用这些字段驱动物理 SIM AKA 适配器。重复参数、畸形头、超限输入在 Rust 解析器中失败关闭。Rust callback 已从 reader session 注入并贯穿 IMS service 生命周期。没有 Rust parser 的旧测试/兼容调用仍沿用原 Go parser，当前生产 callback 缺失时不会静默回落。

SIP socket、TCP stream、响应分派/等待、实体卡 RAND/AUTN AKA APDU、IMS 注册与会话生命周期仍由 Go 持有；本轮不宣称 IMS 全 Rust 化。

验证：Rust `proletarius-ims` AKA authorization 测试 **5 项通过**；Rust daemon parser operation 定向测试 **2 项通过**；Go `simauth`、`voiceclient`、`imscore`、macOS bridge 定向测试通过。完整 `scripts/test-migration-gates.sh` 通过（最终 `migration gates passed`），覆盖 Rust workspace、Go test/vet、Swift BLE、既有协议差分；契约扫描 diff 通过，仍为 **343 method/path（Rust 283 / Go 60）**、127 Swift 调用点、353 review scopes。门禁日志：[iteration-366-migration-gates.log](../build/optimization/iteration-366-migration-gates.log)。

本轮 P3 增计 0.025 计划人周等值；P3 累计 **1.890 / 8.0**。全项目累计 **16.370 / 47.5 = 34.4632%**，显示 **34.5%**（两位小数 **34.46%**），估值误差约 ±3 个百分点。

没有连接实体 SIM/读卡器，没有运营商、短信或通话实测，也未生成 DMG。
