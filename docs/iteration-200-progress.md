# Iteration 200：VoWiFi 操作结果读取迁入 Rust

日期：2026-10-09。改动范围限定在 `/Users/proletarius/Desktop/Switch`。

监督模式下，`GET /api/vowifi/result` 的公开路由和响应校验现在由 Rust 负责。Rust 通过已认证的内部适配器读取 Go 当前持有的异步操作快照，并验证必需字段类型、状态字段与可选字段；保持有效响应的 JSON 形状，并将错误分别映射为无效上游 502 与适配器不可用 503。GET/HEAD 方法契约一致，HEAD 沿用 GET 表示头且不返回正文。显式 Go 兼容启动保留原公开处理器。

底层 VoWiFi 启停、取消/成功竞态、操作状态写入和状态来源仍归 Go 所有。本轮只迁移公开读取边界与契约校验，不代表会话、IMS、IKE 或硬件控制已由 Rust 接管。

验证：

- Rust 定向测试：3 项通过，覆盖旧 wire shape、类型错误 fail-closed、认证 Unix-socket 快照与适配器缺失。
- Go 定向测试：3 项通过，覆盖内部凭证、结果快照和未知字段拒绝。
- `scripts/test-migration-gates.sh`：通过。Rust daemon **1,097 passed / 3 ignored**；Go 全仓测试与 `go vet`、2,722 项载波回放、USB/radio/HTTP method+HEAD/SDP/AMR parity通过。
- 契约清单检查与源码 `--diff` 通过：334 个 method/path 契约（Rust 269、Go 65，含49个GET派生HEAD alias）、127个Swift调用点、298个审阅范围。
- Rust格式检查与 `git diff --check` 通过。
- 未连接读卡器/SIM，未拨号、未发送短信、未访问运营商；未构建App或DMG。

保守增加P2B **0.025计划人周等值**；累计 **14.770/47.5=31.1%**（计划人周估值误差约±3个百分点）。P2A **4.110/5.5（74.7%）**，P2B **5.400/8.0（67.5%）**，P3 **1.640/8.0（20.5%）**，P4 **0.750/4.5（16.7%）**。进度按能力/风险估值，不按路由数量计算。

剩余工作仍包括模块 VoWiFi/IKE/IPsec/IMS 会话执行、N待多 Profile 生命周期、语音/SIP/媒体、读卡器和 eSIM 设备路径、短信设备事务、Go 构建与运行依赖退出，以及实机/运营商与长期稳定性验收。
