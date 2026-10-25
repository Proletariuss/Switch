# Iteration 453：将公开 health 成功语义固定在 Rust

日期：2026-10-13。工作范围仅限 `/Users/proletarius/Desktop/Switch`。本轮未访问读卡器/号卡、未发送短信、未运行运营商网络测试。包含本轮源码的 DMG 为 `dist/Switch-3.15-macOS27-iteration454.dmg`。

## 改动

- Rust 继续拥有 `GET /api/health` 的公开响应投影，并明确在 Go 子进程处于同一 ready generation、观测 DTO 有效且前后 generation 稳定后，才返回 `ok: true`。
- Go 私有健康 observation adapter 现在只提供 generation 绑定的 USB/eSIM 设备事实，不再重复决定公开 `ok` 状态。Go 显式兼容模式的旧公开 handler 不变。
- Rust 兼容解码仍接受旧 Go observation 中多余的 `ok` 字段，但会忽略该值，避免它覆盖 Rust 的成功条件。公开响应字段与含义保持原样。

## 验证

- `cargo fmt --all --manifest-path rust/Cargo.toml --check` 通过。
- `cargo test --locked --manifest-path rust/Cargo.toml -p proletariusd health_projection::tests`：8 项相关测试通过，覆盖旧/新 observation wire shape、可选 USB 数据、缺失字段与 generation fencing。
- `go test ./cmd/proletarius-macos -run 'TestRuntimeHealthSnapshot' -count=1` 通过。
- `python3 scripts/check-rust-migration-contracts.py --diff` 与 `python3 scripts/tests/test_rust_migration_contracts.py` 通过；`git diff --check` 通过。
- Release App `3.15 / build 315` arm64 构建成功，App 签名校验通过；DMG 校验有效。该包为本机 ad-hoc 签名，未做 Developer ID 公证。

## 边界与进度

这次只把 observation/public projection 的成功语义边界收紧；USB discovery 和 eSIM availability 的事实仍来自 Go compatibility adapter，因此 Go build/runtime dependency 未减少，不能算 health capability 已 Go-free。P2A 健康投影和项目总体进度不增加，整体保持 **37.99%（18.045/47.5）**。native lifecycle、protected IMS、live SMS/call gates 和实机验收仍未完成。
