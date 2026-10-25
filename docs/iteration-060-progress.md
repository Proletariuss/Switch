# Iteration 060 — Compose Celmux LPA with Switch reader transport

Date: 2026-10-05  
Goal: Switch 全 Rust 后端改造；不生成 DMG。

## 本轮实现

- 新增 `proletarius-reader::euicc_lpa::open_lpa_client`，把已有 Switch APDU transport 接到 Celmux-derived MIT `proletarius-lpa` 下载状态机：APDU adapter → owned logical channel → `proletarius-euicc::Euicc` → caller-supplied ES9 transport。
- 调用方显式传入 STORE DATA MSS：独立 reader 按现有 Go custom transport 使用 240，模块/QMI 保持既有 120，避免误用 eUICC crate 的 254-byte 通用默认值；分别加入断言。
- 逻辑通道仍由 RAII owner 关闭；打开客户端只申请一个新 channel。SELECT 失败时保持原始卡状态错误并清理本次 channel，不探测/关闭既有 channel，也不回退到基本通道。
- API 不自行打开设备或宣称持有资源。调用方必须为整个 LPA session 持有跨进程 reader lease 和同设备操作锁，避免协议流程与现有 Go/Swift reader 操作交错；这仍是接生产路由前的硬门槛。
- 未复用 Celmux 的 HTTP/DNS/proxy/TLS implementation；ES9 网络能力仍由 Switch `Es9Transport` 显式注入，并沿用 Switch 自己的 trust、响应上限与 redirect 策略。9eSIM `6982`范围不变。

## 验证与边界

- `cargo +1.98 test --offline -p proletarius-reader -p proletarius-lpa -p proletarius-euicc`：52 项通过、0 项失败；随后 `cargo +1.98 test --workspace --offline --quiet`：738 项通过、11 项忽略、0 项失败。
- 覆盖客户端组合/逻辑通道号及 RAII 清理、SELECT 失败前置拒绝，并回归 LPA 假卡/假 ES9 协议序列和 eUICC profile codec。Rustfmt 与 `git diff --check` 通过。
- `cargo +1.98 check --offline -p proletariusd`、workspace fmt check 与 `git diff --check` 通过；daemon 编译仍显示 `native_vowifi_session.rs` 两项既有未读取字段 warning。
- 所有 APDU 与网络均为 fake fixture；未运行 App、接触读卡器/SIM、访问 SM-DP+ 或写入 Profile。
- Rust download API、operation/confirmation、lease heartbeat、蓝牙 reader 路径和 Go route owner 本轮均未迁移。因此只完善下一阶段的 adapter seam，不增加生产接管百分比。

详情见 [全 Rust 实施记录](optimization/2026-10-04-all-rust-execution.md) 与 [Celmux 复用评估](optimization/2026-10-04-celmux-reuse-review.md)。
