# Iteration 459：全量离线门禁与 3.15 DMG 核验

本轮未改动后端业务 owner 或生产代码。重新运行 `scripts/test-migration-gates.sh`：Rust workspace **1,460 passed / 0 failed / 3 ignored**；Go 全仓 `go vet ./...` 与 `go test ./...` 通过；Swift BLE 操作完成证明和 BLE manager **38 组**虚拟事件序列通过；2,722 项 carrier replay，以及 USB 状态、模块 radio、HTTP method/HEAD、IMS SDP/AMR、MESSAGE 路由、USSD 和呼叫 wire parity 均通过。日志中存在 dormant Rust native-session dead-code warnings 与 Go 重复 libusb linker warning，不影响本轮通过结果。

`scripts/check-rust-backend-exit.py` 仍报告 **6 类 Go 退出阻塞**：Xcode 继续编译并嵌入 Go 后端、Rust daemon 启动并转发至 Go、Go module 尚存，以及 **266 个**生产 Go 源文件。Rust 离线测试和 parity 不能证明这些生产 owner 已迁移，也不能证明实体读卡器、运营商网络或真实短信/通话行为。

按用户本次要求已从当前源码构建并核验 `Switch-3.15-macOS27-iteration459.dmg`；App 深度签名校验和 `hdiutil verify` 均通过。此包版本为 **3.15 / build 315**。本轮不计全 Rust 迁移点数，进度保持 **37.99%（18.045/47.5）**。未启动应用、操作读卡器/SIM、发送短信或连接运营商网络；9eSIM `6982` 继续排除。

下一段 owner 迁移仍在 P3/P4：Go `multiStandbyManager` 持有 Profile session 启停；现有 Rust `NativeSessionContext` 又把读卡器 lease/heartbeat 保留到整个 IMS tunnel 生命周期。直接接通该 dormant actor 会使同一实体卡的多个 Profile 争用独占 lease。应先让 Rust 以单一读卡器仲裁器串行提供每次 AKA/APDU 操作，同时由每个 Profile actor独立持有 tunnel/IMS 生命周期，并补齐短信与通话 event handlers；在这些实现和门禁完成前保持生产 native-start gate 关闭。
