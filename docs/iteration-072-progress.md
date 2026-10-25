# Iteration 072：Celmux IMS/IMSA service-client 生命周期适配

日期：2026-10-05。工作范围只涉及 `/Users/proletarius/Desktop/Switch/src`；`/Users/proletarius/Downloads/celmux-src` 仅作只读参考。

## 完成内容

- 在 `proletarius-qmi-services` 添加 Switch-owned `ImsClient`，通过 Celmux-derived QMI core 共用一条由调用方持有的 transport，并分配独立 IMS/IMSA client ID。
- 接入当前 Go QMI enhancement 已使用的 IMS Get/Set 与 IMSA registration 查询；检查响应 service、message、response flag 和 QMI result，不把 indication 当作 response。
- 显式 `false` 设置值与 QMI TLV 次序保持 Go 兼容。
- `ImsClient::close` 尝试释放两个 service IDs。若 IMSA allocation 失败，已创建的 IMS handle 自动回收。
- 用 fake transport 验证双服务 allocation、Get、Set、registration status、逐服务释放、部分 open 失败后的 rollback。
- 在 Celmux 复用评估和第三方通知中记录新增的适配边界。没有复制新的 Celmux 源文件，也没有修改参考目录。

## 验证

- `CARGO_HOME=/Users/proletarius/Desktop/Switch/build/cargo-home CARGO_TARGET_DIR=/Users/proletarius/Desktop/Switch/build/rust-target cargo +1.98 test --offline --locked -p proletarius-qmi-services -p proletarius-qmi-core -p proletarius-qmi-libusb`：16 项通过；本次新增的 IMS client 生命周期用例 2 项通过。新增的直接依赖已写入 `rust/Cargo.lock`。
- `bash scripts/test-migration-gates.sh`：Rust workspace、`switchd-rs` 336 项、Go `go vet`/全包测试，以及 2,722 个 carrier replay、PCMA、SDP、AMR parity 均通过。
- 仍有 `native_vowifi_session.rs` 两项此前存在的 dead-code warning；链接器提示重复 `-lusb-1.0`。完整门禁退出码为 0。
- 测试均使用内存传输和本地 fixtures；没有枚举/打开实体 QMI 设备、启动 App、修改卡片、发送短信或生成 DMG。Rust 1.98 未安装 Clippy component，本轮未运行 Clippy。

## 进度与下一步

生产 QMI owner 仍在 Go：当前增强通道只在 `-backend=qmi` 启用，且 operator 查询还依赖 NAS；`switchd-rs` 尚未把 `proletarius-qmi-libusb` 和 `ImsClient` 放进一个互斥的设备 owner。此轮只是 client facade 与生命周期回归，按既定口径不算生产设备接管，P2B 保持 **22%**，整体保持 **20.0% 已完成 / 80.0% 待完成**（约 ±3 个百分点）。

下一步先按实际 Go 调用面补 NAS operator 查询与可重绑 service owner，然后在显式 QMI backend 中接通 Rust transport/client 与关闭/重连；必须保证 Go 与 Rust 不会同时打开同一 USB interface，并用 Go 脱敏 QMI 轨迹验证行为后才计入迁移进度。9eSIM `6982` 继续排除。
