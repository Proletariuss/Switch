# Iteration 349：普通 USB 短信发送直达 Rust owner

普通 USB AT 设备短信现在由 Rust `device_sms_send` owner 在唯一耐久 claim 后直接提交给 Rust USB AT owner，不再调用 Go `action=execute` 回调。Go route capability 仍用于采集时的状态/身份事实以及有效性校验和释放；modem/serial、IMS 发送路径保持原适配器。

短信路由采集到的 ICCID 必须与 Rust 当前 SIM identity snapshot 一致，发送记录只保存 Rust identity generation 与不透明 Go capability，不落盘 ICCID。实际执行再次核对活跃 SIM lease 与 generation，并由现有 USB AT 事务在同一串口核验 QCCID。切卡、租约失效或身份代次变化时失败关闭；不确定发送继续保持不可重放。测试证明该 USB 路径不会触发 Go execute 回调。

验证结果：

- Rust `device_sms_send` 专项：19 项通过。
- Rust USB AT 当前 SIM lease / generation 专项：2 项通过。
- Go 设备短信/路由专项通过。
- `scripts/test-migration-gates.sh` 完整离线门禁通过：Rust workspace 1,888 passed / 6 ignored（daemon 1,350 passed / 3 ignored）；Go 全包测试与 vet、Swift BLE 回归及现有 carrier/USB/radio/HTTP/codec/SDP/MESSAGE 跨语言差分通过。
- 契约清单通过 `--write-inventory` 与 `--diff`：342 method/path（Rust 282 / Go 60）、127 个 Swift HTTP 调用点、353 个审阅范围。
- Rust format check 与 `git diff --check` 通过。

本轮没有操作实体卡、发送真实短信、连接运营商或生成 DMG。离线测试不能证明真实读卡器/卡片上的发送结果。

P2B 增加 0.025 计划人周等值，累计 **15.970 / 47.5 = 33.6211%**（显示 33.6%，两位小数 33.62%，整体估算误差约 ±3 个百分点）。**31.4% 是 Iteration 209 的历史数值；历史迭代条目仍保留当时快照。**本轮不代表全 Rust 完成：Go 仍保留设备通道事实与 capability 适配、modem/serial 和 IMS 发送、EAP/IMS/VoWiFi 会话以及部分读卡器生命周期。

完整门禁日志：[iteration-349-migration-gates.log](../build/optimization/iteration-349-migration-gates.log)。契约核对日志：[iteration-349-contract-diff.log](../build/optimization/iteration-349-contract-diff.log)。
