# Iteration 444：Clarivate lifecycle findings cross-check and generation stop wiring

日期：2026-10-13

## Clarivate_IOS 交叉核对

- 对 `/Users/proletarius/Desktop/Clarivate_IOS` 只读审阅 `RELEASE_625_ANALYSIS.md` 与 `RELEASE_626_STABILITY.md`。Clarivate 625 的运营商数据停留在 `ios-carrier-profiles` revision `d46fc4e`（683 profiles / 853 selectors）；Switch 已在 Iteration 443 前同步到 `e560437`，selector 展示名更新已在本地，不回退模板或覆盖用户的 `3_hk.json`。
- Clarivate 626 修复的是其 Go/Flutter/iOS reader、IMS 与扫描生命周期，包括关闭已接受的 SIP 连接、读锁异常释放、停止 Future 共享和 BLE 扫描代次。没有照搬平台特定实现；将可迁移的不变量用于核对 Switch：generation stop 必须同时通知 supervisor 和 daemon owner 实际持有的 session task，shutdown 仍等待任务真实完成。
- Clarivate 文档明确 625/626 的实网/真机验收仍待完成；其 IPA 验证不作为 Switch 的 VoWiFi、eSIM、短信或硬件测试证据。

## Switch 改动

- `NativeLifecycle::stop_generation` 之前只通知 `NativeVoWiFiSupervisor`，绕过 Iteration 443 新增的 `NativeSessionServiceOwner::stop_generation`，导致 daemon 持有的 session `Task` 没有收到协作取消信号。
- 现在 lifecycle 将 generation-only stop 委托给 owner；owner 先由 supervisor 验证当前 generation，再通知对应 task。未知/过期 generation 仍返回 `Missing`，不会影响替代会话。
- 新回归测试经真实 `NativeLifecycle` 调用 stop，验证 supervisor 与 owner-held task 的信号路径、task 仍由 owner 持有直到回收，并保留 worker panic 作为失败结果。

## 验证与范围

- 定向 lifecycle 测试通过；`cargo test --locked --manifest-path rust/Cargo.toml -p proletariusd`：**1452 passed / 3 ignored**。
- 全 Rust 生产启动门禁、protected IMS readiness 和 SMS/call runtime gate 维持关闭；本轮未接生产 VoWiFi start/stop API，也不宣称网络、读卡器、实体卡或运营商验收。
- Carrier 数据、运营商身份选择、代理/DNS 路径和 9eSIM `6982` 范围未改变。总体迁移进度不因离线生命周期连线而增加；Go 构建与运行依赖仍存在。
- 未生成 DMG。

## 后续

让真实 session startup 在一个 daemon owner 中持有 immutable context、reader lease、protected IMS/network task、SIP/SMS/call dispatchers 和 cleanup receipt；逐项证明启动输入来源与退出顺序，再考虑接入 API。维持 readiness gates 关闭直至完整离线契约、物理资源取消、重启/恢复与目标设备验收通过。
