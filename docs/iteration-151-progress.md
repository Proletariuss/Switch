# Iteration 151：来电按钮布局与 USB/PCSC AKA、IMS 短信事务 Rust 接入

日期：2026-10-08（Asia/Shanghai）。所有源码与构建产物均在 `/Users/proletarius/Desktop/Switch`；未更改该目录外的原始副本。

## 来电按钮位置

`CallView.liveCallActions` 现在给呼入按钮组提供全宽居中容器；`centeredCallActions` 用居中的 `ZStack` 保持按钮原生宽度，不再用 `GeometryReader.position` 放置控件。接听和挂断作为一个整体相对通话卡片可用宽度居中；拨号按键/挂断状态也复用同一布局。

## Rust 迁移切片

选定且已就绪的 USB/PCSC reader 的公开 `POST /api/reader/auth` 现在由 Rust 处理。它复用现有 Rust AKA、PC/SC transaction 和 generation-bound reader operation lease；保持旧校验顺序、响应字段及错误码投影，并在挑战材料生命周期结束时清零。BLE、未选卡以及 APDU 提交前取得租约失败继续使用旧 Go 兼容处理；提交后绝不回退或重放，Rust worker 保留读卡租约、心跳与确认释放直到事务完成。Go 仍持有 reader generation/session 管理、兼容路径及更高层 VoWiFi 协调，因此本轮没有关闭这些依赖，也不代表 SIMworld 实机 AKA/IMS 测试成功。9eSIM `6982` 继续排除。

本轮还将 IMS 外发短信的 SIP MESSAGE 构造与事务状态接到 Rust：Rust按Call-ID/CSeq/MESSAGE匹配响应，串行推进multipart接受状态，提交后超时/断连标记为unknown且禁止自动重放。Go仍提供已注册IMS TCP连接、SIP事件分发和会话生命周期。新的私有UDS仅返回待发送的SIP帧及事务状态，不表示SIMworld短信已实际发送或送达。

## 验证

- Rust workspace `cargo fmt --all -- --check` 通过；`cargo test --offline --workspace`：**880通过、0失败、3忽略**。只出现既有 `native_vowifi_session.rs` dead-code warning。
- Go 全仓 `go test -timeout=30s ./...` 与 `go vet ./...` 通过。
- 冻结 Go mux HTTP/HEAD 差分通过；日志在 `../../build/optimization/iteration-151/go-http-parity.log` 与 `../../build/optimization/iteration-151/rust-http-parity.log`。
- 迁移合同 `--diff` 通过：281条 method/path（Rust209、Go72，含34个HEAD别名）、127个Swift HTTP调用点、248个审阅范围；合同工具43项自测通过。`--require-rust-only` 按预期失败：默认Go fallback仍存在，143条路由仍有Go所有权或依赖。
- Xcode Debug `Switch.app` 构建成功，2.93/build293 arm64，产物位于 `../../build/DerivedDataCallActionsFix/Build/Products/Debug/Switch.app`；`git diff --check` 通过。未生成DMG。
- 未启动生产应用、枚举/连接读卡器、读取或写入实体卡、发送短信或访问运营商网络。

## 进度

按47.5计划人周加权，P2B及P3本轮各增加 **0.025人周**：累计 **13.295/47.5 = 28.0%**（约±3个百分点），P2B为 **4.775/8.0 = 59.69%**，P3为 **1.465/8.0 = 18.31%**。来电布局纯UI改动不计入迁移比例。整体仍处于混合后端阶段；完整 AKA/VoWiFi、P2B硬件、全Go依赖退出及最终无Go构建验收尚未完成。
