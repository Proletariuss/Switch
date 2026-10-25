# Iteration 183：短信存储用量查询与 eSIM 操作别名迁入 Rust

日期：2026-10-09。源码、构建和测试状态均保留在 `/Users/proletarius/Desktop/Switch/`。

监督模式下，公开 `GET /api/sms/storage` 现在直接由 Rust 网关处理。Rust 从本机归档生成 LOCAL 用量，并通过唯一 USB AT owner 查询实体 SIM/ME 存储；查询与 lease、射频抑制和在途 USB I/O 使用同一准入锁，遇到模块 SIM 操作时返回忙状态，设备不存在时仍提供本机归档用量。旧 Go handler 仅保留 standalone 兼容启动路径。已同步修正超时策略测试：监督模式的短信刷新由 Rust 直接处理，不再要求它遵循 Go 转发超时。`GET /api/esim/operation` 旧别名也直接转到 Rust 已有的 eSIM 操作状态读取逻辑，不新增卡片写入或下载操作。

验证通过：`RUST_TEST_THREADS=1 bash scripts/test-migration-gates.sh` 全量门禁；Rust workspace **1,026 passed / 3 ignored**；Go 全仓测试与 `vet`；2,722 项载波回放；USB 状态、模块射频策略、HTTP method/HEAD、IMS SDP 与 AMR framing parity。默认并行测试时有两项资源竞争测试偶发失败，分别单独复跑均通过；串行全量门禁通过。迁移清单已重新生成：**314 条 method/path 契约，Rust 250、Go 64，含 44 个 GET 派生 HEAD alias**；127 个 Swift HTTP 调用点、290 个审阅范围。迁移扫描器43项自测与 `--diff` 通过。

本轮未访问读卡器或号卡、未执行实体 AT/短信操作，未进行真实设备验收、Go-free 构建或 DMG 构建。Rust 对公开用量读取负责，但 serial/QMI/MBIM 等设备发现和兼容模式仍依赖 Go；因此这不是硬件层、全短信链路或整个后端 Go-free 的完成证明。

保守计入 P2B **+0.025 计划人周等值**，由 5.100 增至 **5.125/8.0（64.1%）**；总体由 14.295 增至 **14.320/47.5（30.1%）**，估算误差约 ±3 个百分点。P2A 为 3.960/5.5（72.0%），P3 为 1.615/8.0（20.2%），P4 为 0.750/4.5（16.7%），eSIM/LPA 为 1.125/5.5（20.45%）。Go 的通话控制/媒体、IMS/SIP 生命周期、reader lease、BLE/CoreBluetooth 生命周期、QMI/MBIM 兼容路径、N待物理/profile session owner，以及 Go-free 发布链仍待迁移和验收。
