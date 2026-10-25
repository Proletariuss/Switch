# Iteration 454：将 IMS REGISTER 授予租期绑定到成功请求

日期：2026-10-13。仅修改 `/Users/proletarius/Desktop/Switch` 源码。本轮不访问读卡器/号卡、不发短信、不进行运营商网络或实机测试。

## 改动

- `RegisteredImsSession` 现在从成功的最终 REGISTER 请求保留 Contact 与 Expires。请求字节通过有界 SIP parser 解码；不是 REGISTER、头缺失、重复/非法形状或消息未完整消费时，不生成可用租期。
- Rust 以该请求的 Contact 绑定和最终 200 响应中的 Contact/Expires 调用已有 lease policy，得到服务器授予的有效期。匹配歧义、绑定不匹配、撤销或非法期限会 fail closed 为“租期未知”，不会猜一个超时值。
- 会话暴露授予秒数和依据现有策略推导的 renewal deadline，供下一步 Rust supervisor 接管。deadline 仅为数据，不会自行刷新 REGISTER、重建 IKE 或关闭连接。
- 更新 API migration census 的整文件人工审阅指纹与审阅说明；路由数量与 ownership 未改变。

## 验证

- `cargo test --locked --manifest-path rust/Cargo.toml -p proletarius-vowifi`：185 个单元测试、2 个载波集成测试通过。
- `cargo check --locked --manifest-path rust/Cargo.toml -p proletariusd`、`cargo fmt --all --manifest-path rust/Cargo.toml --check` 通过。
- `python3 scripts/check-rust-migration-contracts.py --write-inventory` 后 `--diff` 通过；工具回归测试 44 项通过；`git diff --check` 通过。

## 阶段状态与交付边界

这项工作将授予租期从未连接的 Rust policy 接到 native REGISTER 成功响应，但它尚未被生产 IMS actor 用于定时续期/受控重建，native IMS production gate 仍关闭。没有 Go 依赖退出证据、carrier 实测或长稳观察，故 P3/P4 验收门与项目总加权进度均不增加，继续记录为 **37.99%（18.045/47.5）**。本轮代码没有进入 DMG；最近的 `Switch-3.15-macOS27-iteration455.dmg` 构建早于此轮源码修改。
