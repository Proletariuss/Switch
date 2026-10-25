# Iteration 298：租约内 BLE AKA APDU 转入 Rust

在 `NativeReaderSessionLease::authenticate` 中把 BLE 鉴权从 Go 私有 `/api/internal/reader/session-lease/aka` 请求迁到 Rust。Rust 持有现有代次绑定读卡器租约、ICCID/IMSI 快照和 USIM/ISIM 选择，通过已认证 Swift CoreBluetooth Unix 桥逐条提交 APDU，并复用身份绑定 AKA 验证；PC/SC 路径不变。挑战、身份副本与桥接 token 使用零化容器，reader UUID 在桥状态握手及每条 APDU上绑定。确认该 Go endpoint 只有这一个 Rust 生产调用者后，连同 Go handler/路由、manager callback、Rust IPC DTO/fallback 一并退役；Go 仍提供逻辑租约、心跳、释放/确认和切卡 finalizer，因此这不是 Rust 原生读卡器 actor 或完整 VoWiFi session 接管。

重写租约测试，使 Go 合约只覆盖 acquire/renew/release；新增假 Swift Unix 桥完整回放 17 条 AKA APDU响应，并验证握手、token、reader、APDU动作及 RES/CK/IK。测试不访问实体读卡器。目标测试 `reader_session_lease::tests` **12 passed**，Go租约handler测试通过；`scripts/check-rust-migration-contracts.py` 一致性通过，Go AKA endpoint 退役后契约为 **334 method/path（Rust273、Go61）**；`git diff --check` 通过。最终完整迁移门禁通过：Rust workspace **1189 passed / 3 ignored**、Go `vet` 和全包兼容测试、载波/USB状态/Radio/HTTP method-HEAD/IMS差分均通过。Rust-only退出门禁仍失败：默认 Go fallback 尚在，177 条 route 仍含 Go owner/dependency。

工作树中的完整 IMS native readiness gate 仍关闭；目前仅把实际 BLE AKA APDU链迁入Rust，IMS keyed actor接线、BLE/读卡器原生生命周期、短信/语音媒体运行、完整Go-free构建运行和真实设备/运营商验证仍未完成。按生产接管计分原则，总体进度保持 **31.4%（约±3个百分点）**；不生成 DMG，不访问 SIM、不收发短信或拨号。9eSIM `6982` 按用户指示继续不处理。
