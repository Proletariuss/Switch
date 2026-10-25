# Iteration 117 — 全 Rust 契约扫描器性能

日期：2026-10-08。

全项目 API 契约扫描器在本机持续满核运行，停在 Rust 词法解析。定位到 `lex` 对每个 token 都创建 `text[i:]` 字符串切片，再在切片上重跑正则；daemon Rust 源码约2.5 MiB 时，累计复制和匹配成本快速放大，影响每轮迁移后的 P0 清单验证。

修复将 raw-string 前缀、字符字面量和通用 token 正则预编译，并通过 `Pattern.match(source, offset)` 在原始源码上匹配；raw-string 搜索起点改用 match 返回的绝对终点。没有放宽 token 识别规则或变更 API ownership 抽取逻辑。新增20,000行 fixture，断言 raw string 里的 `//` 仍留在字符串 token 中。

验证：

- `PYTHONDONTWRITEBYTECODE=1 /opt/homebrew/bin/python3 scripts/tests/test_rust_migration_contracts.py`：**42项通过**。
- `PYTHONDONTWRITEBYTECODE=1 /opt/homebrew/bin/python3 scripts/check-rust-migration-contracts.py`：**4.88秒通过**；262项路由（Rust182 / Go80，含30个HEAD别名）、208个审阅范围、127个Swift调用点与登记一致。
- 扫描仍明确说明它只是 P0 基线一致性；它不证明 Rust 路由端到端无 Go 依赖。Rust-only 退出门禁仍未满足。

此为 P0 公共测试设施优化，无产品路由变更。按计划工作量计入0.025人周，P0/P1约43.63%，总计划完成估算为12.195/47.5≈25.7%（误差约±3个百分点）。完整后端迁移仍在进行中；未进行实机操作、未生成DMG。
