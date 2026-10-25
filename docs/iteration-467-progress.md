# Iteration 467：USB 设备短信改由 Rust session capability 直达 owner

普通 USB AT 短信发送现在在 Rust capture 阶段转换为绑定当前 USB owner session 和 SIM identity generation 的内部 capability。Go 只提供初始设备/路由观察；Rust 校验该 session/代次仍有效后释放 Go 的短期 route token，durable send intent 不再保存 ICCID。提交前由 Rust 再次检查绑定，claim 成功后经 Rust USB AT owner 执行身份复核、PDU/CMGS 和确认；该原生路径不再调用 Go validate/execute。模块 `DeviceSms` reservation 将 USB 发送与 eSIM/VoWiFi等 SIM 通道工作互斥。既有持久 USB token、serial modem、IMS/reader 和 legacy binding 仍保留兼容路径，Go initial capture 还未移交。

另修复等待超时与真实 USB I/O 分离时的 reservation 生命周期：底层 USB blocking owner 已接纳操作后，HTTP waiter 达到公开预算只收到 `sms_send_outcome_unknown`；Rust detached supervisor 持续等待物理 owner、完成唯一提交账本/归档并确认释放 module reservation。不会因为等待者超时取消 reservation 后台收尾，也不会重放不确定短信。新增确定性测试覆盖 owner task 在 waiter 超时后继续运行到终态；另有本地 binding 无需 Go validate RPC、USB session/generation/lease 失效 fencing 及 reservation 互斥测试。所有设备行为均由假传输测试覆盖，本轮未向实体卡发短信或执行硬件读写。

验证：`cargo +1.98.0 fmt --all -- --check` 通过；`cargo +1.98.0 test --locked -p proletariusd` **1,471 passed / 0 failed / 3 ignored**；短信服务定向 **22 passed**；迁移合同工具 **45 passed**；合同 census 为 **354** 个 method/path（Rust **293**、Go **61**，含51个HEAD别名）、127个Swift HTTP调用点、379个审阅范围，`--diff` 与检查器一致性通过。Go/Swift 源码未在本轮修改；未运行实体硬件或运营商验收。

本项完成 USB AT send capability 和超时后收尾的 Rust owner 转移，但初始路由观察仍依赖 Go，IMS/reader/legacy 与清空硬件路径仍未退出 Go，所以不计新的完整迁移人周，总体仍为 **37.99%（18.045/47.5，误差约±3个百分点）**。全 Rust 后端目标仍未完成。上一轮生成的 `Switch-3.15-macOS27-iteration467.dmg` 早于本轮改动，未包含此处修复；本轮未重新生成 DMG。

测试日志：[`iteration-467-proletariusd-tests.log`](../../build/reports/iteration-467-proletariusd-tests.log)、[`iteration-467-device-sms-tests.log`](../../build/reports/iteration-467-device-sms-tests.log)、[`iteration-467-contract-selftest.log`](../../build/reports/iteration-467-contract-selftest.log)、[`iteration-467-contract-diff.log`](../../build/reports/iteration-467-contract-diff.log)。
