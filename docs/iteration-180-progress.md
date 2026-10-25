# Iteration 180：短信、BLE、IMS与读卡器投影迁入 Rust

日期：2026-10-09。所有源码与验证产物仅位于 `/Users/proletarius/Desktop/Switch/`。

Rust接管短信设备可用状态投影与选择服务；选中 BLE reader 的 eSIM 下载由 Rust LPA/APDU 状态机执行；IMS OPTIONS heartbeat 租约和最终响应关联迁入 Rust。新增 `GET/HEAD /api/reader/info` Rust投影，使用按Go子进程代次绑定的内部认证 adapter 获取物理 reader/USIM观察，Rust完成兼容响应、operator回退和carrier-registry ePDG选择。Go仍提供硬件/会话事实，保留reader lease/VoWiFi收尾、SIP生命周期和真实设备操作；手动 `POST /api/at` 仍由Go持有。

完整离线门禁通过：Rust workspace格式和测试通过，daemon **1,017通过/3忽略**；Go全仓 `vet`/测试、2,722项载波回放及USB/radio/HTTP method+HEAD/codec+SDP parity通过；Rust/Go监督进程集成 **30项**通过。契约工具43项自测与 `--diff` 通过；清单313条（Rust247/Go66，43个HEAD alias）、127个Swift HTTP调用点、290个审阅范围。未做实机、Go-free构建运行或发布验收；无设备/运营商操作，未构建App/DMG。

保守计入P2A **+0.025**至3.960/5.5（72.0%）；P2B **+0.050**至5.075/8.0（63.4%）；P3 **+0.025**至1.590/8.0（19.9%）。总体由14.145升至**14.245/47.5（30.0%）**，误差约 ±3 个百分点。短信会话、reader lease、VoWiFi、SIP与硬件生命周期仍有Go依赖。