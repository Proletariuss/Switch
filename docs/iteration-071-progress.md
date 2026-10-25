# Iteration 071：Switch-owned macOS QMI/libusb transport

## 完成内容

在 Celmux-derived `proletarius-qmi-core` 的 `Transport` 接缝上新增
`rust/crates/proletarius-qmi-libusb`。设备筛选与 QMI lifecycle transport
由 Switch 自己实现；没有复制 Celmux 或 Go 的 transport 源文件。

- Auto 设备顺序与 Go 一致：DJI `2ca3:4006` 优先，其次 Quectel `2c7c:0125`；显式 VID:PID 支持 `0x` 前缀。
- 只接受 vendor-specific `0xff` interface；排除 ADB `ff/42/01`。DJI 额外要求 `ff/ff/ff`、interrupt 和双向 bulk endpoint；Quectel 保留 Go 的通用 vendor-specific bulk 规则。
- 对候选 interface 做 claim；非零 alternate setting 会显式切换；只有 QMI CTL_SYNC 成功后才返回 client。
- transport 将 bulk read 的 100ms poll 与 Go 的 3 秒 write budget 保持有界；读写可并发，关闭时先阻止新操作、等待在途 I/O 结束，再释放 interface。进程内 owner lease 防止重复打开。
- 新增锁定依赖 `rusb` 0.9.4，按 macOS target 编译；打包脚本现在会将 libusb 的 loader-relative 引用校验同时应用于 Go 与 Rust 后端。第三方 MIT attribution 已加入 `THIRD_PARTY_NOTICES.md`。

## 验证与限制

- `cargo +1.98 test --offline --locked -p proletarius-qmi-libusb -p proletarius-qmi-core`：7 项通过，包括 4 项 selector/filter tests 和 3 项 QMI fake-transport lifecycle tests。
- `bash scripts/test-migration-gates.sh` 通过 Rust workspace、`switchd-rs` 336 项、Go vet/全包测试、2,722 carrier replay 及 PCMA/SDP/AMR parity。
- `cargo +1.98 fmt --all -- --check`、`sh -n scripts/build-backends.sh` 与 `git diff --check` 通过。
- 当前 Rust 1.98 安装没有 `clippy` component，故本轮未运行 Clippy；transport crate 在 workspace 编译及测试中没有产生 Rust compiler warning。

所有新测试都不枚举真实设备。`open_qmi_client` 虽已实现扫描、claim、CTL_SYNC 和 I/O，但尚未被 `switchd-rs` 调用；也没有 Go golden QMI session replay、物理模块验收或错误恢复测试。Go 仍然是实际 QMI/MBIM owner，构建仍包含 Go，因此此轮**不增加全 Rust 后端进度**：保持 **20.0%（9.51 / 47.5 计划人周，误差约 ±3 个百分点）**、P2B 22%。下一阶段把 Rust client 接入现有 QMI enhancement 调用面，按合成 Go 轨迹逐服务核对，并迁移 close/reconnect 与 APDU arbiter 生命周期；只有完成单一 Rust owner 和生产验收后才计分。9eSIM `6982` 继续排除；未启动 App、连接读卡器或生成 DMG。
