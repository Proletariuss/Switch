# Iteration 411：外呼号码目标规范化迁入 Rust

生产拨号路径现在通过现有认证 IMS codec endpoint 调用 Rust，将国际 `00` 号码规范化为 `+` 形式，并按 carrier phone-context、home domain、IMS realm 顺序解析本地号码。Rust 清理复制的目标与上下文输入；Go adapter 校验返回值并保留既有错误语义。Go 仍负责 SIP 会话/事务和网络 I/O；没有新增 HTTP 路由或设备副作用。

Rust 单测、daemon operation 测试、Go adapter/parity 测试通过；完整迁移门禁通过：Rust workspace 1,420 passed / 3 ignored，Go vet/全包测试、Swift BLE 回归与协议差分通过。契约 diff 与 44 项扫描器自测通过；清单为 346 method/path（Rust 286 / Go 60）、127 Swift 调用点、368 审阅范围。门禁日志：[iteration-411-migration-gates.log](../build/optimization/iteration-411-migration-gates.log)，SHA-256 `c763218fd781b8332dd456553142d4411575db2df22da3249bc6821f59faaf86`。

P3 +0.025，累计 **2.765 / 8.0（34.56%）**；全项目 **17.470 / 47.5 = 36.78%**。Release build 308 已编译，签名校验和 DMG 镜像验证通过：[Switch-3.08-macOS27-iteration411.dmg](../dist/Switch-3.08-macOS27-iteration411.dmg)，SHA-256 `32a30b90e280ba9033721fe4cf4ae194308abc8a038a5daca9597916a57eb08c`。全 Rust 退出门禁仍在 7 个构建/运行/源码类别受阻；本轮没有 IMS/运营商实机验收。详见[进度复核账本](optimization/all-rust/progress-reassessment-2026-10-10.md)。
