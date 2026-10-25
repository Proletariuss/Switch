# Iteration 160：BLE 通知读取与重试路径统一到 Rust

日期：2026-10-08。所有代码、审阅记录和验证材料均在 `/Users/proletarius/Desktop/Switch`。本轮未连接实体读卡器/SIM，未访问运营商网络，未构建 App 或 DMG。

选中 BLE reader 的通知列表和手动重试现在进入 Rust，并经 Swift CoreBluetooth 桥把 APDU 绑定到被冻结的 BLE reader UUID。USB/PCSC 与 BLE 共用同一 transport-based LPA 列表、自动清理、通知投递/确认和重试实现。Go 继续负责 generation-bound reader lease、选择代次复核、heartbeat/finalizer 与缓存；无可用选中 reader 时仍转发旧 Go 路径。没有新增公开 HTTP 路由。

新增 fake bridge 离线测试：列表测试驱动空通知列表；重试测试驱动指定序号无待处理内容。两者都核对每个 APDU 带有选中 reader UUID 和认证 token。它们没有测试 BLE 上的实际 ES9+ 投递，也不代表实体卡或运营商验收。

验证通过：`scripts/test-migration-gates.sh`（Rust daemon **939 项通过、3 项忽略**；Go 全仓测试与 `go vet`；2,722 项 carrier replay；USB、radio、codec 与 HTTP/HEAD parity）；Rust/Go 独立进程集成 **17 项**；迁移契约工具 **43 项**及 `--diff`；Rustfmt 与 `git diff --check`。契约清单仍为 **291** 条 method/path（Rust 223、Go 68，含37个HEAD alias）、127个 Swift HTTP 调用点；新增两项人工审阅范围后共270项。Rust-only退出门禁仍被默认 Go fallback 及148条 Go owner/dependency route阻断。

保守计入 eSIM/LPA **+0.025 计划人周等值**，从 **1.100/5.5（20.00%）**提升至 **1.125/5.5（20.45%）**；总量由 **13.570/47.5**提升到 **13.595/47.5≈28.6%**，估算误差约±3个百分点。P2B维持4.925/8.0，P2A维持3.785/5.5，P3维持1.540/8.0，P4维持0.475/4.5。未生成 DMG。
