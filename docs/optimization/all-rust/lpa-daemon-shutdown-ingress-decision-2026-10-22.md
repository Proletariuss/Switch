# LPA 守护进程退出联接：合成入口审议

2026-10-22。结论：**仍为 UNKNOWN；本轮不新增守护进程级 LPA 写卡/退出回归。** 没有实际卡操作、短信、通话或 DMG，也不授予功能 PASS。

生产 `main.rs:1119,1198–1204` 在退出时先关闭 eSIM 接纳，再等待 `reader_esim_download::wait_for_shutdown()`，然后执行模块 finalizer。`reader_esim_download.rs:441–462` 等待共享 operation gate；`2200–2240` 的 retained owner 在 lease release、代次绑定的刷新和终态写入后才释放 gate。`reader_esim_download.rs:7147–7184,7236–7281` 已用进程内 `Service` 和模拟的 `shutdown::coordinate` 证明取消 HTTP waiter 后的 gate drain 与 finalizer 顺序，但没有启动真实 shipping daemon、发送退出信号或重启读取 journal。因此不能据此关闭 `runtime-owner-format-exit-map.json` 的 “LPA daemon-shutdown join remains unverified” 与 `remaining_LPA_unknown`。

现有 shipping 进程测试 `rust/crates/proletariusd/tests/native_process_tests.rs:1–79` 启动真实二进制，但把 `PROLETARIUS_BLE_SOCKET` 设为不存在的 socket；`scripts/check-p6-data-roundtrip.py:90–96` 也如此。BLE 假 APDU 服务及 `connect_for_test` 分别位于 `reader_esim_download.rs:5676–5748` 与 `reader_ble_apdu.rs:308–316` 的 `cfg(test)` 路径，不能由 shipping 子进程直接使用。生产环境变量可指向一个 Unix BLE bridge，但仓库中没有现成的、可在强制禁硬件沙箱下完成 BLE 连接、读卡器选择、LPA 写入及写后刷新阻塞的 shipping 进程夹具。

静态路径还存在需要独立核实的准入落差：`reader_inventory.rs:85–99` 把 bridge 报告的 BLE reader 加入 discovery 时标记 `CardPresence::Unknown`；`reader_esim_download.rs:1025–1053` 仅把同一 discovery 中 `CardPresence::Present` 的选中 reader 放行到 eSIM 路由；`proletarius-reader/src/lib.rs:1010–1023` 的 reconcile 保留观测到的 card presence。现有原生 HTTP 测试 `reader_pcsc_gateway_tests/native_lpa_routes.rs:100–113` 则通过 `cfg(test)` 的 `test_seed_selection` 和手工 `Present` 观测建立入口。上述静态差异不足以断言实体 BLE 功能结果，却说明当前夹具不能原样迁移到 shipping 子进程。

为只验证 daemon join 而新增生产调试入口、伪造卡存在或重写 BLE 选择/发现逻辑，会改变实际准入边界，超出本次有限验收工作的成本和授权。后续可单独审议 BLE discovery 的存在性语义及合成 bridge 的完整协议夹具；只有安全获得已接纳 LPA 写入后，才适合做真实进程退出、终态 journal、重启回读的阻塞测试。P0–P6 总体仍未完成，分母未冻结。
