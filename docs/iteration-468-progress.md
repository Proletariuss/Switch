# Iteration 468：普通模块短信 capture 按 Rust ModuleRadio transport 直连 owner

普通模块短信的准备阶段现在可在 Rust 内完成初始 owner capture：当目标是 `dji`、没有显式 subscription、VoWiFi 未占用 SIM 通道时，按当前 ModuleRadio transport 选择 Rust serial modem 的 SMS generation，或 USB AT 的 active session+SIM identity generation，并要求 USB owner session 与 ModuleRadio 配置 session 精确相同。配置为 USB 时不再误用可能残留的 serial generation或不同会话的 USB lease。USB/serial owner 已有的单次 claim、发送、耐久归档和未知结果不可重放逻辑继续生效；该路径不调用 Go capture/validate/execute。

Reader、显式 subscription、VoWiFi 活动或 Rust owner 当前不可用时保留既有 Go capture 兼容路径，错误/业务语义不做推断。此轮只证明离线分支与项目回归，不证明实体卡或运营商短信。设备短信路径仍有 Go 兼容分支，故不增加完整迁移权重，整体维持 **37.99%（18.045/47.5，误差约±3个百分点）**；Go 退出审计依旧为 6 类阻塞、266 个生产 Go 文件；Rust-only route gate 仍有178条契约依赖 Go。全 Rust目标未完成。

验证：`cargo +1.98.0 fmt --all -- --check`、`git diff --check`；完整 `proletariusd` **1,474 passed / 0 failed / 3 ignored**（短信服务专项25项通过）；迁移合同自测45项通过，`--diff`通过，census仍为354条（Rust293/Go61，含51 HEAD aliases）、127个Swift HTTP调用点、379个审阅范围。测试均为离线测试。

合同人工审阅已补记并刷新库存。`Switch-3.15-macOS27-iteration468.dmg` 是上轮显式打包所得，早于本轮 transport 选择修正，**不含本轮最新代码**；本轮未重建 DMG。

日志：[proletariusd tests](../build/reports/iteration-468-proletariusd-tests.log)、[contract diff](../build/reports/iteration-468-contract-diff.log)、[contract self-test](../build/reports/iteration-468-contract-selftest.log)。
