# Iteration 131 — 呼入按钮居中与 Rust ADB 运行时部署

日期：2026-10-08。全 Rust 后端目标继续进行中；本轮 UI 修复单独处理，不计入迁移进度。

## 呼入按钮布局

来电时“接听 / 挂断”按钮组此前通过 `GeometryReader` 在通话卡片内绝对定位。卡片位于横向 `ScrollView` 中，读到的几何宽度可能小于卡片可见宽度，所以按钮组偏左。本轮改为固定按钮组宽度、让父级填满可用宽度并用居中对齐；通话中操作组也使用同一类对齐方式。

Release SwiftUI 编译通过，`DeviceCallCorrectness` 离线回归通过。没有模拟真实呼入或操作实体设备。

## Rust ADB 部署归属

通话运行时准备由 Go 先后向 Rust 查询完整文件校验，再发送经内部令牌和 `SWITCH_CALL_PRODUCER_ID` 绑定的部署请求。Rust 新增精确 `POST /api/internal/call-mode/adb/deploy-runtime`：在 blocking worker 中检查 producer generation，读取并校验六个清单文件，再获取共享 `.module-adb-interface.lock`，连接 libusb ADB，验证 root shell、目标内核和远程目录，最后只向模块传输清单标记的三个部署文件。

Rust 直接从 no-follow 文件句柄读取哈希正确的本地工件，并使用 ADB SYNC 的 `SEND/DATA/DONE` 事务推送，不经 base64 HTTP 中转。单次部署设 120 秒上限；producer 在开锁前、接触 USB 前及每个文件传输前重新检查。远端拒绝、错流 ACK、损坏包、文件篡改、锁冲突及未知返回均失败关闭。

Go `module_voice_darwin.go` 删除了本地 ADB root/kernel/目录检查及 `adb push` 文件循环；`adb_darwin.go` 删除 `push` 和 SYNC 写入实现，旧 `/runtime/deployment-files` 路由、Go base64 客户端及重复部署子清单一并移除。Go 仍重开 ADB 执行既有音频驱动、helper、自检和通话模式后续流程；这些设备侧工作仍未迁移。

## 验证与限制

- Rust daemon：**799 通过、0 失败、3 忽略**。新增 transcript 测试覆盖 ADB 分片、同步包字节、远端文件模式、DONE 时间戳、错误 ACK 和设备 `FAIL`。
- Go `cmd/proletarius-macos` race 测试通过，`go vet` 通过；迁移清单一致性通过：269 条 method/path（Rust 189、Go 80，含 30 条 HEAD alias）、127 个 Swift HTTP 调用点、221 个审阅范围。
- `DeviceCallCorrectness`、Rustfmt 检查及 `git diff --check` 通过。
- `scripts/build.sh` Release 构建通过，`dist/Switch.app` 为 2.86 / build 286 / arm64，deep strict 代码签名验证通过。未生成 DMG。
- 未操作读卡器、实体 SIM 或模块；未发起呼叫、发送短信或加载模块驱动。ADB 部署协议只由离线 transcript 验证，真实设备兼容性仍待后续授权硬件验收。

验证日志：`/Users/proletarius/Desktop/Switch/build/optimization/iteration131/`。

## 进度记账

P2B 计入 **+0.100 计划人周等值**，由 4.175 增至 **4.275/8.0（53.44%）**；总体由 12.645 增至 **12.745/47.5≈26.8%**，估算误差约 ±3 个百分点。只计 Rust ADB 部署与 Go 文件推送退出及离线验证；不计本轮 UI 修复、实机验收或剩余 Go 通话模式流程。

下一段继续迁移模块侧驱动/运行时检查与恢复路径，并核验 Rust/Go USB 句柄交接、失败关闭和状态机边界；在真实硬件验证前，不把离线 transcript 结果当作实体设备已通过。
