# Iteration 457：移除未知 API 路径的隐式 Go 转发

Rust API 网关现在只把契约中明确登记的 61 条 Go 兼容路由转发给 Go。其他未知/未来路径，以及已声明为 Rust 但没有实际 Rust handler 的请求，均由 Rust 返回 `404 route_not_found`。HEAD 延续同一路径已登记 GET 的归属，不单独增加兼容路由数。

契约审计器解析 Rust 的字面 method/path allowlist，并逐项与 Go AST 路由清单比较；缺少登记、额外登记或重复项都会失败。45 项审计器回归测试覆盖新增 Go 路由、兼容路由表解析及未登记路径。保留了 eSIM 健康检查在未选中读卡器时明确调用 Go 的兼容回退；选中读卡器仍走 Rust wrapper。

验证：Rust daemon `cargo test --locked -p proletariusd`、45 项契约工具测试、契约 `--diff`、Rust format check、`git diff --check`。Rust-only 退出门禁按预期失败：仍有显式 Go 转发，且 178 条契约路由带有 Go owner/dependency。全量状态为 354 条 method/path（Rust 293 / Go 61，含 51 个 HEAD alias）、127 个 Swift 调用点、379 个审阅范围。此次路由封闭加固不移除业务 Go owner，不增加全 Rust 加权进度，保持 **37.99%（18.045/47.5）**。

本轮未启动 app、Go 后端、读卡器、SIM、短信或运营商网络，也未生成 DMG。现有 Iteration 456 DMG 不包含本轮源码变更。
