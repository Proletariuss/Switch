# Iteration 382：REGISTER 候选路由决策迁入 Rust

日期：2026-10-12。

生产 IMS REGISTER 现在通过现有 authenticated IMS codec 调用 Rust，将 P-CSCF/ePDG 候选展开为有序 registrar/transport 对。Rust 负责候选去重、本地 IP 地址族优先、当 registrar 是 UE 本地地址时把它映射到已排序的 ePDG gateway，并保留现有 3HK IPv6 gateway 排序规则。解析和排序不做 DNS、socket 或网络 I/O。生产 `reader_vowifi_session` 注入 resolver；REGISTER attempt 与后续 IPsec 路由投影使用同一个 Rust 已选 gateway，避免生产路径重新执行 Go 排序。

Go 仍负责 SIP candidate 拨号、实际响应/超时判断、尝试间隔与重试、SIP 事务、物理 AKA、IPsec transform 和 IMS 生命周期。若 Rust UDS 缺失、输入/输出无效或 Rust 拒绝候选，生产 REGISTER 失败关闭。独立 imscore fixture 保留未配置 callback 时的旧 Go helper，供兼容测试使用。无新增 HTTP 路由，P0 清单仍为 343 method/path（Rust 283 / Go 60）。

验证：Rust IMS candidate tests **3 passed**，daemon codec integration test **1 passed**；Go imscore 和 macOS bridge tests 通过。完整迁移门禁通过：[iteration-382-migration-gates.log](../build/optimization/iteration-382-migration-gates.log)，Rust workspace **1,953 passed / 6 ignored**（daemon **1,387 passed / 3 ignored**），Go 全包 test/vet、Swift BLE proof/manager regressions 和 carrier/USB/radio/HTTP method+HEAD/codec/SDP/AMR/MESSAGE parity 通过。契约 census **343 method/path（Rust 283 / Go 60）**、127 Swift HTTP 调用点、356 审阅范围；`--diff`、审计器 **43 tests**、Rustfmt 与 `git diff --check` 通过。日志 SHA256：`b50281c23613695ab0225ab124584d93829abbe6ab56fb027bb5c8f7fb1ef127`。

本轮计入 P3 **+0.025 计划人周等值**，累计 **2.265 / 8.0**；项目累计 **16.745 / 47.5 = 35.2526%**（显示 **35.3%**，两位 **35.25%**，估值误差约 ±3 个百分点）。不做真实 SIM/运营商网络测试，未生成 DMG。下一步继续迁移活跃 IMS 注册/会话状态所有权与持久在线恢复，而非进一步计分未激活的 voiceclient 路径。
