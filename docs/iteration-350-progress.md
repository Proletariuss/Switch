# Iteration 350：modem/serial 短信执行移交 Rust

普通 modem/serial 短信现在由 Rust `device_sms_send` owner 在唯一 durable claim 后直接提交给 Rust modem actor，不再执行 Go `action=execute` 回调。Go 仍提供 route capture/validate/release capability 和原始 route facts。准备阶段读取 actor 当前 SMS generation，并将代次与 opaque Go binding 一起固定在发送意图中；actor 在任何 AT 写入前再次校验代次。`EncodingPolicy::Modem` 与原 Go `smsSubmitOptions` 保持一致。

执行时 Rust `module_radio` 获取互斥 `DeviceSms` reservation，并持有至 modem actor 返回；活动 VoWiFi intent 失败关闭，eSIM/通话/诊断等 Rust owner 使用相同 reservation 状态。Rust modem actor 将 CMGF、全部 CMGS prompt、segment 和最终接受响应作为原子队列事务，避免与其他 AT work 交错。

公开等待上限保持 180 秒；超时返回 unknown 且不允许重放。已 claim 的 modem 事务转入后台 Rust supervisor，继续持有 radio reservation，待 actor 最终确认后才归档/finish 并释放 Go route capability。actor 未确认、generation 变化或部分接受均不能被报告为已发送。

验证：

- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` 通过。
- Rust `device_sms_send` 专项 20 项通过；DeviceSms reservation 与其他模块事务互斥专项 1 项通过；Rust modem SMS atomicity/generation 专项 4 项通过。
- Go device-SMS route/send 专项与 `go vet ./cmd/proletarius-macos` 通过。
- `scripts/test-migration-gates.sh` 完整离线门禁通过：Rust workspace 1,887 passed / 6 ignored（daemon 1,352 passed / 3 ignored），Go 全包测试/vet、Swift BLE 回归及现有 carrier/USB/radio/HTTP/codec/SDP/MESSAGE 差分通过。
- 契约清单 `--write-inventory` 与 `--diff` 通过：342 method/path（Rust 282 / Go 60）、127 Swift HTTP 调用点、353 审阅范围。
- `git diff --check` 通过。

本轮未访问真实读卡器、未发送短信、未连接运营商或生成 DMG。离线测试不能证明 SIM 上实际短信投递结果。

按统一尺度，P2B 增加 0.025 计划人周等值，累计 **15.995 / 47.5 = 33.6737%**（显示 33.7%，两位小数 33.67%，估算误差约 ±3 个百分点）。Go 仍保留 route facts/capability 生命周期、IMS 发送、VoWiFi/IMS 会话、模块物理生命周期及显式兼容启动；本轮不代表全 Rust 完成。

完整门禁日志：[iteration-350-migration-gates.log](../build/optimization/iteration-350-migration-gates.log)。契约核对日志：[iteration-350-contract-diff.log](../build/optimization/iteration-350-contract-diff.log)。
