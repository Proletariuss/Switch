# Iteration 158：BLE eSIM 操作与 N 待列表投影迁入 Rust

日期：2026-10-08。源码、审阅记录及构建/测试产物均限定在 `/Users/proletarius/Desktop/Switch`。本轮未制作 DMG，未连接读卡器/SIM，也未建立真实 IMS 或运营商连接。

选中的 BLE 读卡器现在可以经 Rust 完成 eSIM Profile 改名和启用 APDU 序列；Rust 逐条通过现有 Swift CoreBluetooth bridge 提交 APDU，并持有 Go generation-bound reader mutation lease。操作前复核当前选择，租约丢失后停止后续 APDU；切卡后的 VoWiFi 协调、读卡器会话 admission/finalizer 和 CoreBluetooth 系统桥仍分别由 Go/Swift 承担。USB/PCSC 现有路径保持不变，9eSIM `6982` 不处理。

普通 reader/module 的 `decode_and_archive_incoming_delivery` 在本轮开始前已由既有 Rust transaction 提供；本轮没有改动或重复计算 IMS。Go 仍管理 SIP stream/session、立即 SIP 200 与 RP-ACK 网络传输；N 待入站仍使用 Iteration 157 的 profile-scoped Rust store。

`GET /api/multistandby/profiles` 的公开列表和离线 Profile/desired-state 投影迁至 Rust。Rust读取耐久 catalog/revision，并把已认证 Go runtime snapshot 作为只读的实时观察输入；当 Go 运行态暂不可用时返回服务错误，不谎报实时在线。原 GET/HEAD 兼容规则保留。Go 仍管理多 Profile 活跃会话、卡仲裁和恢复状态。

迁移契约扫描为 **290 条 method/path**（Rust **221**、Go **69**，含 **36** 个 GET 派生 HEAD alias）、127 个 Swift HTTP 调用点和265个审阅范围。独立 method/path 为 Rust185、Go69。计数是路由归属，不等同于端到端 Go-free。

验证通过：`scripts/test-migration-gates.sh` 全量离线门禁、Rust workspace（`proletariusd` **937通过、3忽略**）、Go 全仓测试与 `go vet`、2,722 项 carrier replay、USB/radio/HTTP-HEAD parity、Rustfmt、`git diff --check`；迁移契约扫描 `--diff` 和 **43 项**工具自测通过。Rust 与 Go 真实子进程隔离集成 **16 项**通过，覆盖 Rust profile projection、既有 N 待存储与 IMS archive 路径、重启恢复。fake BLE bridge 的改名/切卡 APDU 序列测试也包含在 Rust daemon 套件中。

Rust-only 退出门禁仍被默认 Go fallback 和 **145 条** Go owner/dependency route 阻断。上述测试均为离线或 fake bridge 证据，不代表实体 BLE/PCSC、运营商 VoWiFi/SMS 或长期在线验收。没有重建 App，也没有生成 DMG。

按计划权重计入本轮两个小切片：P2A **+0.025人周**至 **3.785/5.5（68.82%）**；P2B **+0.025人周**至 **4.900/8.0（61.25%）**。P3维持 **1.540/8.0（19.25%）**，P4维持 **0.475/4.5（10.56%）**。总体为 **13.545/47.5 = 28.5%**，估算误差约 ±3 个百分点；待完成 **71.5%**。这不代表语音媒体、IMS session 生命周期、多 Profile 原生仲裁、真实设备验收或 Go-free 构建已经完成。
