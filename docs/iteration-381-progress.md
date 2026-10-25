# Iteration 381：legacy voiceclient REGISTER 策略接入 Rust（不加权）

日期：2026-10-12。

voiceclient 的遗留 REGISTER challenge retry loop 现在能通过现有 authenticated IMS codec 调用 Rust 决策继续或停止。决策收到实际 challenge 的算法名、完成次数及 legacy 两轮上限；主 IMS 的 plain-digest 提前停止规则关闭，以保留 voiceclient 原先行为。Rust 错误时该已配置路径失败关闭；未配置 callback 的独立测试/嵌入方式仍保持两轮回退。

生产路径核查：`imscore.Service.attachMessaging` 是唯一生产 `voiceclient.Dial` 调用点，并继续设置 `SkipRegister: true`。因此主 REGISTER 不会进入该循环，本轮只是兼容/休眠路径治理，不是新增生产接管，不增加项目进度。整体基线保持 **16.720 / 47.5 = 35.2%（35.20%）**；31.4% 是 Iteration 209 历史值，不是当前值。

验证：Go voiceclient、imscore、macOS backend 定向测试通过；Rust IMS policy **4 passed**，daemon codec **1 passed**。完整迁移门禁通过：[iteration-381-migration-gates.log](../build/optimization/iteration-381-migration-gates.log)，Rust workspace **1,949 passed / 6 ignored**（daemon **1,387 passed / 3 ignored**），Go 全包 test/vet、Swift BLE proof/manager tests、carrier/USB/radio/HTTP method+HEAD/codec/SDP/AMR/MESSAGE parity 通过；契约清单 **343 method/path（Rust 283 / Go 60）**、127 Swift 调用点、356 审阅范围，审计器 **43 tests**、契约 `--diff`、Rustfmt 和 `git diff --check` 通过。日志 SHA256：`4ecaba0664dbfb527c0ea3b8b261105dfc20bb816294555e0b0a8427dfcece2c`。

无实机/运营商验收，无 DMG。下一步应优先迁移生产 IMS 注册状态机、stream/transaction 与会话生命周期中仍由 Go 持有的真实 owner，而不是继续扩展当前未激活的 voiceclient 注册路径。
