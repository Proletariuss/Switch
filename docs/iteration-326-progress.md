# Iteration 326：读卡器事务仲裁退出 Go

2026-10-10。源码生产接线与完整离线门禁通过；没有进行实卡读写、短信、电话测试或生成 DMG。发行包仍是冻结至 Iteration 322 的 2.97。

## 接管与保留

Rust 现在持有普通读卡、生命周期转换、在线鉴权和 N 待卡片借用的事务准入、不可变能力、续期、到期、释放及退出静默屏障。Go `authMu` 及依赖布尔 owner 的绕过路径删除，全部消费方必须显式传入同一个事务上下文。N 待只能借用其精确业务父能力，不能凭读卡器名称取得权限。

BLE APDU/reset、PC/SC open/acquire/transmit/reset，以及 PC/SC AKA/EAP 在排队前验证生产者、原始事务绑定和当前有效 reader/generation。实际 worker 持有守卫；HTTP 取消、响应丢失、Go 退出或租约超时都不证明卡操作完成。释放先阻止新 I/O，再由 Rust 保留的清理任务等实际 worker/物理终态；未到达 PC/SC worker 的 acquire 也有防重入高水位。

生命周期提交只推进自身事务的有效代次，原始能力保持不变。未知 prepare 在实际清理完成后退休并失效旧身份；已提交的新选择不因回复丢失回滚。原生切卡 finalizer 即使收到取消，仍以精确绑定和唯一序号完成已开始的收尾，结束后旧能力不能继续访问卡片。

Go 临时 LPA 管理器只捕获本次事务，不把短租约工厂放回持久管理器；缓存按读卡器、代次、管理器指针及缓存版本校验后回写，禁止短作用域启动后台重读。临时新增 `backend/internal/esim/reader_scope.go` 是兼容期缓存桥，因此生产 Go 文件数由 258 变为 259；文件数量不参与完成率。

Go 仍保留六条窄 pin/finalize 适配、物理连接/复位/身份与缓存投影、部分 LPA 消费者，以及 IMS/VoWiFi、N 待网络会话和媒体生命周期。本轮不能称作整个 reader owner 或完整后端已退出 Go。

## 验证

- `scripts/test-migration-gates.sh` 通过：Rust workspace **1,841 passed / 6 ignored**，daemon **1,312 passed / 3 ignored**；Go vet 和全部 **26 个测试包**通过，4 个包无测试；Swift 实际 BLE Unix 桥和 **38 组** manager 事件序列通过；carrier/USB/radio/HTTP/codec/SDP/MESSAGE 差分全部通过。
- Go 主包 **703 个顶层测试**通过；最终整个主包 `-race -count=1` 通过（15.650 秒，`iteration-326-go-command-race.log`）。此前 manager/operation/lifecycle/pin/presence **93 个顶层竞态测试**、PCSC/EAP/presence 定向 race 和 eSIM 全包 race 也通过。
- Rust 专项包含仲裁 **62 项**、BLE gateway **10 项**、PCSC gateway **11 项**、auth/EAP 真实 Unix HTTP + 模拟物理 owner **3 项（两接口共 6 条路径）**。取消后只执行一次 AUTHENTICATE，直到实际 worker 结束才解除占用。
- 契约清单 **341 method/path（Rust 280 / Go 61）**、127 Swift 调用点、355 审阅范围、49 个 HEAD 别名；扫描器 **43 项**自测通过。
- Go 退出结构检查仍报告 **7 类依赖、259 个生产 Go 文件**。没有 App 安装或本轮设备验收，不能把离线通过解释为实机通过。

最终门禁日志：`Switch/build/optimization/iteration-326-migration-gates.log`。契约与专项日志使用同目录 `iteration-326-*` 前缀。首轮失败日志保留：旧 BLE gateway 请求未带事务却期待旧退出错误；现改为严格请求校验，并额外验证退出时合法结构但失效的能力不会接触蓝牙。Go 首轮旧夹具生产者不匹配、native finalizer 误用 pin 能力等均在最终验证前修正。

## 进度与下一项

本轮只把独立的“普通读卡事务仲裁与原有 Go 锁退出”计入 P2B **+0.100 计划人周等值**；不重复计算此前原生租约、业务准入、逻辑代次、BLE 物理传输或 AKA 算法。累计 **15.345 / 47.5 ≈ 32.3%**，估算误差约 ±3 个百分点；这是职责迁移估计，不是代码行数或实机验收比例。

下一里程碑：迁移仍在 Go 的 reader 物理生命周期/身份初始化和窄 pin 适配，完成完整 reader owner 退出；随后继续既有 IMS/IKE、N 待网络会话、语音和零 Go 联调门禁。当前总目标保持进行中。
