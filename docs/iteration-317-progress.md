# Iteration 317：BLE 控制操作接入 Rust reader actor

Rust `reader_reservation::Gate` 现在提供全局 BLE-control permit。Rust BLE scan/connect/disconnect handler 在请求 Swift CoreBluetooth bridge 前先获取 permit；permit 存活期间 Rust PC/SC worker、generation-bound operation lease 与 N待 reservation 均拒绝新 admission。permit 使用 RAII：正常结束显式确认释放，bridge/Go admission 返回错误或请求 future 被取消时也会释放。Rust handler 仍调用 Go 的短期兼容租约，以检查 Go 独占的旧 VoWiFi/N待会话；因此 BLE adapter 尚未达到零 Go 依赖。

验证：Rust `reader_reservation` 定向测试 **11 passed**（新增全局互斥、释放和无效 token 场景）；`reader_ble_control` 定向测试 **4 passed**；`cargo fmt --all` 已运行。两项测试覆盖 gate 状态和 BLE bridge 输入/输出兼容，但未覆盖正在运行的 Go VoWiFi 与 CoreBluetooth 的跨进程实机竞争，也未验证 Swift BLE 实卡。

这是 reader-session 内一项实际运行时准入迁移，其他 Rust 内部读卡任务现在与 BLE 控制共用 Rust gate。Go 仍持有 global operation/native-session/N待状态和兼容租约，`reader_session`/`reader_ble_controls` 仍标记需要 Go。保守总进度仍记 **31.4%**，待统一 reader actor 完成并通过 Go/Swift 并发差分后再计完整 owner 转移。未运行全量迁移门禁、未访问实体卡/运营商、未生成 DMG。详见 [Iteration 317](iteration-317-progress.md)。
