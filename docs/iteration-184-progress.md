# Iteration 184：公开短信刷新入口迁入 Rust

日期：2026-10-09。源码、构建、测试日志均保留在 `/Users/proletarius/Desktop/Switch/`。

监督模式下，公开 `POST /api/sms/refresh` 现由 Rust 网关处理。模块串口扫描通过 Rust modem actor 异步排队；没有可用串口时调用唯一 Rust USB SMS owner，并等待有界服务确认后返回既有 `202 {accepted,count}` 语义；VoWiFi 准备、运行或释放期间仍返回 `module_vowifi_owns_sim`。刷新路由不调用公开 Go handler，Go handler 继续保留给显式 standalone 兼容启动。

本轮核对并修正了忙状态边界：刷新只在模块 VoWiFi intent 持有 SIM 安全通道时拒绝；其他射频 reservation 不再被误报为 VoWiFi 占用。既有短信存储读取继续使用较宽的 SIM/射频忙状态，避免查询与在途操作冲突。没有连接读卡器、访问实体号卡或发送短信。

验证通过：Rust workspace **1,026 passed / 3 ignored**；Go 全仓测试与 `vet`；2,722 项载波回放；USB 状态、模块射频、HTTP method/HEAD、SDP 与 AMR framing parity；迁移扫描器43项自测与 `--diff`；`git diff --check`。迁移契约仍为314条（Rust250、Go64，含44个 HEAD alias）、127个 Swift HTTP 调用点和290个审阅范围。

保守计入 P2B **+0.025 计划人周等值**，由5.125增至 **5.150/8.0（64.4%）**；总体由14.320增至 **14.345/47.5（30.2%）**，估算误差约±3个百分点。P2A为3.960/5.5（72.0%），P3为1.615/8.0（20.2%），P4为0.750/4.5（16.7%）。完整 Go-free 构建/运行、真实设备验收及 App 发布仍未完成；未构建 DMG。
