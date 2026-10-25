# Iteration 116 — 呼入按钮对齐与 Rust 诊断事件接管

日期：2026-10-08。

呼入时“接听”和“挂断”按钮此前使用自适应网格，卡片较宽时按钮落在首列，视觉上偏左。现改为固定宽度的水平按钮组，并在可用通话卡片宽度内居中；拨号与进行中通话操作也沿用同一行式布局。

Rust 后端现在独占诊断安全事件的字段校验、序号、256条环形历史及查询。公开 `GET /api/diagnostics/events` 保留 App 认证、HEAD 和旧方法错误语义；内部 `POST /api/internal/diagnostics/events/append` 由 UDS 内部 token 保护，仅接收白名单枚举事件和受限关联 ID，不接受未知字段、原始错误内容或超限请求。Go 生产者经单 worker 与512条有界队列异步发送脱敏事件，队列压力只丢弃最旧待处理诊断项，不阻塞业务。

验证：

- `scripts/test-device-call-correctness.sh`：PASS。
- `cargo +1.98 test --locked -p proletariusd`：739 passed、0 failed、3 ignored。
- Go `cmd/proletarius-macos` 与 `internal/safeevents` race 测试、`go vet`：通过。
- 跨语言多待集成测试：15项通过，覆盖公开授权和内部追加拒绝路径。
- Rust API迁移合同工具：41项自测通过；常规契约清单扫描通过，当前262条（Rust182 / Go80，含30个HEAD别名）、208个审阅范围、127个Swift调用点。Rust-only门禁仍按预期失败：147条路由尚有Go所有权或依赖。
- `cargo fmt --check`、`git diff --check`、Release `scripts/build.sh` 与 `codesign --verify --deep --strict`：通过；产物为2.86/build286 arm64 `Switch.app`。

计划进度：P2A按新增诊断事件 Rust owner、受限路由和异步 Go producer bridge 增加0.035计划人周，达到3.660/5.5（66.55%）；全项目12.170/47.5≈25.6%（估算误差约±3个百分点）。呼入布局不重复计入 Rust 迁移完成率。完整 Go-free 构建、其他 Go 硬件业务和IMS/VoWiFi实机验证仍未完成。未操作读卡器或实体卡，未生成DMG。
